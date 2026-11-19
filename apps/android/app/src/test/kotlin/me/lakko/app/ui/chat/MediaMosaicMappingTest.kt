// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.chat.previewThumbOf
import me.lakko.app.media.InlineKind
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.MediaMosaic
import uniffi.lakko_ffi_uniffi.MediaSize
import uniffi.lakko_ffi_uniffi.MosaicTile
import uniffi.lakko_ffi_uniffi.ThumbRef

/**
 * docs/ux/media-messages.md section 11 — how the shared rule's tiles become
 * Compose placements, which corners round with the bubble, which attachments
 * the mosaic takes, and the list row's thumbnail.
 */
class MediaMosaicMappingTest {

    private val allowlist: (String) -> InlineKind? = { mime ->
        when {
            mime.startsWith("image/") && mime != "image/heic" -> InlineKind.IMAGE
            mime == "video/mp4" -> InlineKind.VIDEO
            mime == "audio/mp4" -> InlineKind.AUDIO
            else -> null
        }
    }

    private fun ref(
        mime: String?,
        kind: String? = null,
        width: UInt? = null,
        height: UInt? = null,
        thumb: ThumbRef? = null,
        tag: Int = 0,
    ) = AttachmentRef(
        ByteArray(32) { tag.toByte() }, ByteArray(32), ByteArray(12), mime, 10u, null,
        kind = kind, width = width, height = height, thumb = thumb,
    )

    private fun thumb(w: UInt?, h: UInt?) = ThumbRef(ByteArray(32) { 9 }, ByteArray(32), ByteArray(12), "image/jpeg", 100u, w, h)

    @Test
    fun `three tiles map to offsets and only outer corners round`() {
        // One large on the start side, two stacked on the end side.
        val mosaic = MediaMosaic(
            tiles = listOf(
                MosaicTile(0u, 0u, 172u, 200u),
                MosaicTile(174u, 0u, 86u, 99u),
                MosaicTile(174u, 101u, 86u, 99u),
            ),
            width = 260u,
            height = 200u,
        )
        val p = tilePlacements(mosaic)
        assertEquals(TilePlacement(0, 0, 172, 200, TileCorners(true, false, true, false)), p[0])
        assertEquals(TilePlacement(174, 0, 86, 99, TileCorners(false, true, false, false)), p[1])
        assertEquals(TilePlacement(174, 101, 86, 99, TileCorners(false, false, false, true)), p[2])
    }

    @Test
    fun `a single tile rounds all four corners and a row of two splits them`() {
        val one = MediaMosaic(listOf(MosaicTile(0u, 0u, 200u, 150u)), 200u, 150u)
        assertEquals(TileCorners(true, true, true, true), tileCorners(one.tiles[0], one))
        val two = MediaMosaic(listOf(MosaicTile(0u, 0u, 129u, 120u), MosaicTile(131u, 0u, 129u, 120u)), 260u, 120u)
        assertEquals(TileCorners(true, false, true, false), tileCorners(two.tiles[0], two))
        assertEquals(TileCorners(false, true, false, true), tileCorners(two.tiles[1], two))
    }

    @Test
    fun `a tile is laid out by its size, else its thumbnail's, else unknown`() {
        assertEquals(MediaSize(4000u, 3000u), mosaicSizeOf(ref("image/jpeg", width = 4000u, height = 3000u, thumb = thumb(320u, 240u))))
        assertEquals(MediaSize(320u, 240u), mosaicSizeOf(ref("image/jpeg", thumb = thumb(320u, 240u))))
        assertEquals(MediaSize(0u, 0u), mosaicSizeOf(ref("image/jpeg", width = 0u, height = 10u)))
        assertEquals(MediaSize(0u, 0u), mosaicSizeOf(ref("image/jpeg")))
    }

    @Test
    fun `the mosaic takes photos and videos in order, at most ten, the rest stays`() {
        val photo = ref("image/jpeg", tag = 1)
        val video = ref("video/mp4", tag = 2)
        val file = ref("application/pdf", tag = 3)
        val voice = ref("audio/mp4", kind = "voice", tag = 4)
        val gif = ref("image/gif", kind = "gif", tag = 5)
        val heic = ref("image/heic", tag = 6)
        val split = splitForMosaic(listOf(file, photo, voice, video, gif, heic), allowlist)
        assertEquals(listOf(photo, video), split.media)
        assertEquals(listOf(file, voice, gif, heic), split.others)

        val many = List(12) { ref("image/png", tag = it) }
        val capped = splitForMosaic(many, allowlist, max = 10)
        assertEquals(many.take(10), capped.media)
        assertEquals(emptyList<AttachmentRef>(), capped.others)
    }

    @Test
    fun `the row thumbnail is the first photo or video, stacked by count`() {
        val t = thumb(320u, 240u)
        val one = previewThumbOf(listOf(ref("image/jpeg", thumb = t)))!!
        assertEquals(1, one.count)
        assertEquals(0, one.stackEdges)
        val two = previewThumbOf(listOf(ref("application/pdf"), ref("video/mp4", thumb = t), ref("image/png")))!!
        assertEquals(2, two.count)
        assertEquals(true, two.video)
        assertEquals(1, two.stackEdges)
        assertEquals(2, previewThumbOf(List(5) { ref("image/png") })!!.stackEdges)
        assertNull(previewThumbOf(listOf(ref("application/pdf"))))
        assertNull(previewThumbOf(listOf(ref("image/jpeg"), ref("audio/mp4", kind = "voice"))))
        assertNull(previewThumbOf(emptyList()))
    }

    @Test
    fun `the viewer shows a position line only for several items`() {
        assertNull(viewerPositionArgs(0, 1))
        assertEquals(mapOf("n" to "2", "total" to "5"), viewerPositionArgs(1, 5))
    }
}
