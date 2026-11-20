// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import me.lakko.app.media.ChatMediaHub
import me.lakko.app.media.MediaAllowlist
import me.lakko.app.media.MediaBubble
import me.lakko.app.media.renderDecision
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.MediaMosaic
import uniffi.lakko_ffi_uniffi.MediaSize
import uniffi.lakko_ffi_uniffi.MosaicTile

/*
 * docs/ux/media-messages.md section 11 — the photos and videos of one message
 * laid out as one mosaic in its bubble. WHERE each tile goes is the shared
 * rule `media_mosaic` in `lakko-ui-rules` (the same numbers on every client);
 * this file only maps its tiles onto Compose and draws them.
 */

/** The bubble width the mosaic fills, in dp (section 10's media width). */
public const val MOSAIC_BUBBLE_WIDTH_DP: Int = 260

/** The radius of the mosaic's outer corners, the bubble's own. */
public const val MOSAIC_CORNER_DP: Int = 18

/** The gap between tiles (`mediaMosaicGap()`; 2 when the core is not loaded). */
internal fun mosaicGap(): UInt = runCatching { uniffi.lakko_ffi_uniffi.mediaMosaicGap() }.getOrDefault(2u)

/** The most items the mosaic shows (`mediaMosaicMaxItems()`; 10 without the core). */
internal fun mosaicMaxItems(): Int = runCatching { uniffi.lakko_ffi_uniffi.mediaMosaicMaxItems().toInt() }.getOrDefault(10)

/** A message's attachments split for the bubble: the photos and videos of
 *  the mosaic (the first [max], in the order sent) and every other one. */
public data class MosaicSplit(val media: List<AttachmentRef>, val others: List<AttachmentRef>)

/**
 * Which attachments go into the mosaic: those whose bubble is a photo or a
 * video (MIME on the inline allowlist and no `kind`, section 10). Beyond
 * [max] photos and videos nothing of the rest is shown.
 */
public fun splitForMosaic(attachments: List<AttachmentRef>, allowlist: MediaAllowlist, max: Int = 10): MosaicSplit {
    val media = ArrayList<AttachmentRef>()
    val others = ArrayList<AttachmentRef>()
    for (ref in attachments) {
        when (renderDecision(ref, allowlist).bubble) {
            MediaBubble.PHOTO, MediaBubble.VIDEO -> if (media.size < max) media += ref
            else -> others += ref
        }
    }
    return MosaicSplit(media, others)
}

/** Section 11 — the size the rule lays an item out by: its own `width` /
 *  `height`, else its thumbnail's, else unknown (0, laid out as 4:3). */
public fun mosaicSizeOf(ref: AttachmentRef): MediaSize {
    val w = ref.width
    val h = ref.height
    if (w != null && h != null && w > 0u && h > 0u) return MediaSize(w, h)
    val tw = ref.thumb?.width
    val th = ref.thumb?.height
    if (tw != null && th != null && tw > 0u && th > 0u) return MediaSize(tw, th)
    return MediaSize(0u, 0u)
}

/** Which corners of a tile are corners of the whole mosaic, so they round
 *  with the bubble; every inner corner is square. Start / end follow the
 *  layout direction, as the rule's `x` does. */
public data class TileCorners(val topStart: Boolean, val topEnd: Boolean, val bottomStart: Boolean, val bottomEnd: Boolean)

public fun tileCorners(tile: MosaicTile, mosaic: MediaMosaic): TileCorners {
    val start = tile.x == 0u
    val end = tile.x + tile.width >= mosaic.width
    val top = tile.y == 0u
    val bottom = tile.y + tile.height >= mosaic.height
    return TileCorners(
        topStart = top && start,
        topEnd = top && end,
        bottomStart = bottom && start,
        bottomEnd = bottom && end,
    )
}

/** A tile as Compose places it: its offset from the top-start corner and its
 *  size, in dp. `Modifier.offset` mirrors x in a right-to-left layout, which
 *  is exactly the rule's "measured from the start edge". */
public data class TilePlacement(val xDp: Int, val yDp: Int, val widthDp: Int, val heightDp: Int, val corners: TileCorners)

public fun tilePlacements(mosaic: MediaMosaic): List<TilePlacement> = mosaic.tiles.map { tile ->
    TilePlacement(tile.x.toInt(), tile.y.toInt(), tile.width.toInt(), tile.height.toInt(), tileCorners(tile, mosaic))
}

/** The mosaic for [refs], from the shared rule; `null` when the core cannot
 *  answer or lays out nothing. */
internal fun layoutMosaic(refs: List<AttachmentRef>, widthDp: Int = MOSAIC_BUBBLE_WIDTH_DP): MediaMosaic? = runCatching {
    uniffi.lakko_ffi_uniffi.mediaMosaic(widthDp.toUInt(), mosaicGap(), refs.map(::mosaicSizeOf))
}.getOrNull()?.takeIf { it.tiles.size == refs.size && it.tiles.isNotEmpty() }

/** The shape a tile is clipped to: the bubble's radius on outer corners. */
internal fun tileShape(corners: TileCorners, radiusDp: Int = MOSAIC_CORNER_DP): RoundedCornerShape {
    val r = radiusDp.dp
    val z = 0.dp
    return RoundedCornerShape(
        topStart = if (corners.topStart) r else z,
        topEnd = if (corners.topEnd) r else z,
        bottomEnd = if (corners.bottomEnd) r else z,
        bottomStart = if (corners.bottomStart) r else z,
    )
}

/**
 * The mosaic of a message: each tile a photo or video bubble of its own
 * (section 10's states per tile), and a tap opens the viewer over every photo
 * and video of the message. [overlay] (the time pill) is drawn over the whole
 * mosaic when the message has no text.
 */
@Composable
internal fun MediaMosaicView(
    refs: List<AttachmentRef>,
    /** [layoutMosaic] of [refs]; `null` draws one bubble per item. */
    mosaic: MediaMosaic?,
    hub: ChatMediaHub,
    onSave: (AttachmentRef) -> Unit,
    onLongPress: (() -> Unit)?,
    /** The bubble's lower corners are its own (text or chips follow below). */
    squareBottom: Boolean,
    /** A quote or forward line sits above it. */
    squareTop: Boolean = false,
    overlay: (@Composable BoxScope.() -> Unit)?,
) {
    var viewerAt by remember { mutableStateOf<Int?>(null) }
    if (mosaic == null) {
        // The core could not lay it out: one bubble per item, as before.
        refs.forEachIndexed { index, ref ->
            MosaicTileView(ref, hub, onSave, onLongPress, null, RoundedCornerShape(MOSAIC_CORNER_DP.dp)) { viewerAt = index }
        }
    } else {
        Box(Modifier.size(mosaic.width.toInt().dp, mosaic.height.toInt().dp)) {
            tilePlacements(mosaic).forEachIndexed { index, place ->
                val corners = place.corners.let { c ->
                    if (squareBottom) c.copy(bottomStart = false, bottomEnd = false) else c
                }.let { c -> if (squareTop) c.copy(topStart = false, topEnd = false) else c }
                Box(Modifier.offset(place.xDp.dp, place.yDp.dp)) {
                    MosaicTileView(
                        ref = refs[index],
                        hub = hub,
                        onSave = onSave,
                        onLongPress = onLongPress,
                        size = DpSize(place.widthDp.dp, place.heightDp.dp),
                        shape = tileShape(corners),
                    ) { viewerAt = index }
                }
            }
            overlay?.invoke(this)
        }
    }
    viewerAt?.let { start ->
        MediaViewer(
            items = refs.map { ref -> viewerItemOf(ref, hub, onSave) },
            startIndex = start,
            onClose = { viewerAt = null },
        )
    }
}

@Composable
private fun MosaicTileView(
    ref: AttachmentRef,
    hub: ChatMediaHub,
    onSave: (AttachmentRef) -> Unit,
    onLongPress: (() -> Unit)?,
    size: DpSize?,
    shape: RoundedCornerShape,
    onOpen: () -> Unit,
) {
    when (renderDecision(ref, hub.allowlist).bubble) {
        MediaBubble.VIDEO -> VideoBubble(ref, hub, onSave, onLongPress, overlay = null, tileSize = size, shape = shape, onOpen = onOpen)
        else -> PhotoBubble(ref, hub, onSave, onLongPress, overlay = null, tileSize = size, shape = shape, onOpen = onOpen)
    }
}

/** A message's photo or video as a viewer page: decrypted on demand. */
internal fun viewerItemOf(ref: AttachmentRef, hub: ChatMediaHub, onSave: (AttachmentRef) -> Unit): ViewerItem {
    val bubble = renderDecision(ref, hub.allowlist).bubble
    val mime = ref.mime?.substringBefore(';')?.trim()?.lowercase()
    return ViewerItem(
        key = ChatMediaHub.mediaKey(ref.blobId),
        isVideo = bubble == MediaBubble.VIDEO,
        animated = mime == "image/gif" || mime == "image/webp",
        load = { hub.fileFor(ChatMediaHub.mediaKey(ref.blobId)) ?: hub.load(ref) },
        save = { onSave(ref) },
    )
}
