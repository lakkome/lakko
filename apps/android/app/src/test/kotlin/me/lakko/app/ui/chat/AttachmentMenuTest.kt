// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.ArticleBlock
import uniffi.lakko_ffi_uniffi.ArticleHeadingLevel
import uniffi.lakko_ffi_uniffi.AttachmentMenuDisabled
import uniffi.lakko_ffi_uniffi.AttachmentMenuItem
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.SharedLocation

/** ADR-0090 / attachments UX — the menu's words, the editors' rules, the cards' formatting. */
class AttachmentMenuTest {

    private val catalog: Map<String, String> by lazy {
        val root = java.io.File("../../../l10n/catalog/en.json").takeIf { it.exists() }
            ?: java.io.File("../../l10n/catalog/en.json")
        val obj = kotlinx.serialization.json.Json.parseToJsonElement(root.readText()) as kotlinx.serialization.json.JsonObject
        (obj["messages"] as kotlinx.serialization.json.JsonObject).mapValues { (_, v) ->
            ((v as kotlinx.serialization.json.JsonObject)["text"] as kotlinx.serialization.json.JsonPrimitive).content
        }
    }

    @Test
    fun `every menu row reads the words the UX spec gives, in the spec's order`() {
        val labels = AttachmentMenuItem.values().map { catalog[attachmentMenuLabelKey(it)] }
        assertEquals(
            listOf("Photo or Video", "Camera", "File", "Audio", "Poll", "Checklist", "Location", "Contact", "Article"),
            labels,
        )
    }

    @Test
    fun `a disabled row says why`() {
        assertEquals("No camera on this device", catalog[attachmentMenuDisabledKey(AttachmentMenuDisabled.NO_CAMERA)])
        assertEquals("Unavailable during a call", catalog[attachmentMenuDisabledKey(AttachmentMenuDisabled.CAMERA_IN_CALL)])
        assertEquals("Location isn't available on this device", catalog[attachmentMenuDisabledKey(AttachmentMenuDisabled.NO_LOCATION)])
    }

    @Test
    fun `Create needs a question and two options, and a correct one in a quiz`() {
        assertFalse(pollDraftReady("", listOf("a", "b"), false, null))
        assertFalse(pollDraftReady("Q", listOf("a", ""), false, null))
        assertTrue(pollDraftReady("Q", listOf("a", "b", ""), false, null))
        assertFalse(pollDraftReady("Q", listOf("a", "b"), true, null))
        assertTrue(pollDraftReady("Q", listOf("a", "b"), true, 1))
        assertFalse(pollDraftReady("Q", listOf("a", "b", ""), true, 2))
    }

    @Test
    fun `a poll kind line names anonymity and quiz`() {
        assertEquals("poll.kind.anonymous_quiz", pollKindKey(anonymous = true, quiz = true))
        assertEquals("poll.kind.anonymous", pollKindKey(anonymous = true, quiz = false))
        assertEquals("poll.kind.quiz", pollKindKey(anonymous = false, quiz = true))
        assertEquals("poll.kind.public", pollKindKey(anonymous = false, quiz = false))
    }

    @Test
    fun `a share rounds to a whole percent and no voters is zero`() {
        assertEquals(0, pollPercent(0, 0))
        assertEquals(33, pollPercent(1, 3))
        assertEquals(67, pollPercent(2, 3))
        assertEquals(100, pollPercent(4, 4))
    }

    @Test
    fun `coordinates read with hemispheres and five decimals`() {
        assertEquals("52.37022° N, 4.89517° E", formatCoordinates(523702200, 48951700, java.util.Locale.ROOT))
        assertEquals("33.86880° S, 151.20930° E", formatCoordinates(-338688000, 1512093000, java.util.Locale.ROOT))
        assertEquals("12 m", formatDistance(12, java.util.Locale.ROOT))
        assertEquals("1.4 km", formatDistance(1400, java.util.Locale.ROOT))
    }

    @Test
    fun `a location opens as a geo link at its point`() {
        val loc = SharedLocation(523702200, 48951700, 12u, 0L, null)
        assertEquals("geo:52.3702200,4.8951700?q=52.3702200,4.8951700", geoUriOf(loc))
    }

    private fun ref(): AttachmentRef = AttachmentRef(ByteArray(32), ByteArray(32), ByteArray(12), "image/jpeg", 1uL, null)

    @Test
    fun `the article editor's blocks become the wire's blocks and back`() {
        val image = ref()
        val blocks = listOf(
            EditBlock(EditKind.HEADING, "Title one"),
            EditBlock(EditKind.TEXT, "Hello", bold = true, links = listOf("site" to "https://x.test")),
            EditBlock(EditKind.BULLET, "a"),
            EditBlock(EditKind.BULLET, "b"),
            EditBlock(EditKind.NUMBER, "c"),
            EditBlock(EditKind.TEXT, ""),
            EditBlock(EditKind.IMAGE, "cap", image = image),
            EditBlock(EditKind.DIVIDER),
        )
        val (article, images) = toArticle(" T ", blocks)
        assertEquals("T", article.title)
        assertEquals(listOf(image), images)
        assertTrue(article.blocks[0] is ArticleBlock.Heading && (article.blocks[0] as ArticleBlock.Heading).level == ArticleHeadingLevel.H1)
        assertTrue(article.blocks[1] is ArticleBlock.Paragraph && (article.blocks[1] as ArticleBlock.Paragraph).runs.size == 2)
        val bullets = article.blocks[2] as ArticleBlock.ListBlock
        assertFalse(bullets.ordered)
        assertEquals(2, bullets.items.size)
        assertTrue((article.blocks[3] as ArticleBlock.ListBlock).ordered)
        assertTrue(article.blocks[4] is ArticleBlock.Image)
        assertEquals(ArticleBlock.Divider, article.blocks[5])
        assertEquals(6, article.blocks.size)
        val back = editBlocksOf(article, images)
        assertEquals(blocks.filter { it.text.isNotEmpty() || it.kind == EditKind.DIVIDER || it.image != null }.map { it.kind }, back.map { it.kind })
    }
}
