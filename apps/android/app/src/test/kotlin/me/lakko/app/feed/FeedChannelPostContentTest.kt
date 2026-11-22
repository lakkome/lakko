// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import me.lakko.app.chat.PersistedAttachment
import me.lakko.app.chat.from
import me.lakko.app.ui.feed.channelPostForwardMessage
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.AttachmentRef
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.MessageContent

/**
 * ADR-0063 / ADR-0092 — a channel post's files and content reach the feed's
 * log, the feed item, and a forward from the feed.
 */
class FeedChannelPostContentTest {
    private val now = 1_800_000_000_000L
    private val id = "0123456789abcdef0123456789abcdef"
    private val channel = FeedSource(FeedSourceKind.CHANNEL, id, "News", muted = false, audience = 0)

    private val photo = AttachmentRef(
        blobId = ByteArray(32) { 1 },
        key = ByteArray(32) { 2 },
        nonce = ByteArray(12) { 3 },
        mime = "image/jpeg",
        size = 10uL,
        name = null,
        kind = null,
        durationMs = null,
        waveform = null,
        width = 640u,
        height = 480u,
        thumb = null,
    )
    private val stored: PersistedAttachment = PersistedAttachment.from(photo)
    private val storedMedia = PersistedChannelPostMedia(mime = photo.mime, kind = photo.kind, ref = stored)
    private val location = MessageContent.Location(
        uniffi.lakko_ffi_uniffi.SharedLocation(latE7 = 1, lonE7 = 2, accuracyM = 3u, fixAtMs = 4L, name = null),
    )

    @Test
    fun theFeedKeepsFilesAndLocationCardArticlePostsAndSkipsPollsAndChecklists() {
        val envelope = "{}".toByteArray()
        val chat = feedPostOutcome(MessageBodyKind.CHAT, "", listOf(storedMedia), envelope) as PostOutcome.Text
        assertEquals(listOf(storedMedia), chat.media)
        assertNull(chat.contentEnvelopeB64)
        assertEquals(PostOutcome.Skip, feedPostOutcome(MessageBodyKind.CHAT, "", emptyList(), envelope))
        for (k in listOf(MessageBodyKind.LOCATION, MessageBodyKind.CONTACT_CARD, MessageBodyKind.ARTICLE)) {
            val kept = feedPostOutcome(k, "", emptyList(), envelope) as PostOutcome.Text
            assertEquals(java.util.Base64.getEncoder().encodeToString(envelope), kept.contentEnvelopeB64)
        }
        assertEquals(PostOutcome.Skip, feedPostOutcome(MessageBodyKind.POLL, "", emptyList(), envelope))
        assertEquals(PostOutcome.Skip, feedPostOutcome(MessageBodyKind.CHECKLIST, "", emptyList(), envelope))
        // docs/ux/message-actions.md section 9 — a delete takes its post out
        // of the log; one naming nothing is passed over.
        assertEquals(PostOutcome.Delete(7), feedPostOutcome(MessageBodyKind.DELETE, "", emptyList(), envelope, target = 7))
        assertEquals(PostOutcome.Skip, feedPostOutcome(MessageBodyKind.DELETE, "", emptyList(), envelope))
    }

    @Test
    fun syncStoresTheFilesAndTheEnvelopeAndAnOldRowReadsWithout() = runTest {
        val deps = object : ChannelSyncDeps {
            override suspend fun fetchPage(channelIdHex: String, after: Long, limit: Int) =
                ChannelPostPage(listOf(ChannelPostWire(1, now, "w1")).filter { it.cursor > after })

            override suspend fun decode(channelIdHex: String, payloadB64: String): PostOutcome =
                PostOutcome.Text("", listOf(storedMedia), "ZW52")
        }
        val slot = syncChannelSlot(id, null, deps, now)
        val post = slot.posts.single()
        assertEquals(listOf(storedMedia), post.media)
        assertEquals("ZW52", post.contentEnvelopeB64)
        val json = Json { ignoreUnknownKeys = true }
        val old = json.decodeFromString(
            ChannelPostLog.serializer(),
            """{"version":1,"channels":{"$id":{"cursor":1,"posts":[{"cursor":1,"serverTsMs":$now,"text":"t"}]}}}""",
        )
        val oldPost = old.channels.getValue(id).posts.single()
        assertTrue(oldPost.media.isEmpty())
        assertNull(oldPost.contentEnvelopeB64)
    }

    private fun ctx(posts: List<PersistedChannelPost>) = FeedContext(
        sources = listOf(channel),
        groupMessages = emptyMap(),
        channelPosts = mapOf(id to ChannelPostSlot(1, posts)),
        ownChannels = emptySet(),
        ownUsername = null,
        contactKeysB64 = emptySet(),
        mentions = { _, _ -> false },
        nowMs = now,
        attachmentRef = { photo },
        parseContent = { if (it == "ZW52") location else null },
    )

    @Test
    fun aChannelItemCarriesItsFilesAndContentAndAForwardTakesBoth() {
        val input = buildFeedInput(ctx(listOf(PersistedChannelPost(1, now, "", listOf(storedMedia), "ZW52"))))
        val ref = input.refs.single()
        assertTrue(input.candidates.single().hasMedia)
        assertEquals(photo, ref.message.attachments.single().ref)
        assertEquals(location, ref.message.content)
        val forwarded = channelPostForwardMessage(ref.message)
        assertEquals(listOf(photo), forwarded.attachments)
        assertEquals(location, forwarded.content)
        assertNotNull(forwarded.createdAt)
    }
}
