// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FeedCandidate
import uniffi.lakko_ffi_uniffi.FeedConversation
import uniffi.lakko_ffi_uniffi.FeedEntry
import uniffi.lakko_ffi_uniffi.FeedPreset
import uniffi.lakko_ffi_uniffi.FeedRule

/** ADR-0063 — assembling the ranking's input and mapping its answer back. */
class FeedModelTest {
    private val now = 1_800_000_000_000L
    private fun iso(ms: Long) = java.time.Instant.ofEpochMilli(ms).toString()

    private val group = FeedSource(FeedSourceKind.GROUP, "aa".repeat(32), "Team", muted = false, audience = 4)
    private val channel = FeedSource(FeedSourceKind.CHANNEL, "bb".repeat(16), "News", muted = false, audience = 0)

    private fun msg(id: Long, dir: String, text: String = "m$id", at: Long = now, vararg extra: (FeedMessage) -> FeedMessage): FeedMessage {
        var m = FeedMessage(messageId = id, direction = dir, text = text, createdAt = iso(at))
        for (f in extra) m = f(m)
        return m
    }

    private fun ctx(
        groupMessages: List<FeedMessage>,
        posts: List<PersistedChannelPost> = emptyList(),
        ownChannels: Set<String> = emptySet(),
    ) = FeedContext(
        sources = listOf(group, channel),
        groupMessages = mapOf(group.idHex to groupMessages),
        channelPosts = mapOf(channel.idHex to ChannelPostSlot(posts.maxOfOrNull { it.cursor } ?: 0, posts)),
        ownChannels = ownChannels,
        ownUsername = "anna",
        contactKeysB64 = setOf("CONTACT"),
        mentions = { text, name -> text.contains("@$name") },
        nowMs = now,
    )

    @Test
    fun groupRowsBecomeCandidatesWithTheirSignals() {
        val input = buildFeedInput(
            ctx(
                listOf(
                    msg(1, "outgoing", "mine"),
                    msg(2, "incoming", "hi @anna", extra = arrayOf({ it.copy(authorKeyB64 = "CONTACT") })),
                    msg(3, "incoming", "re", extra = arrayOf({ it.copy(replyTarget = 1L) })),
                    msg(4, "system"),
                    msg(5, "incoming", extra = arrayOf({ it.copy(deleted = true) })),
                    msg(6, "incoming", at = now - FEED_MAX_WINDOW_MS - 1),
                    msg(
                        7,
                        "incoming",
                        extra = arrayOf({
                            it.copy(
                                reactions = listOf(FeedReaction("a", "x"), FeedReaction("a", "y"), FeedReaction("b", "x")),
                                attachments = listOf(FeedAttachment("gif", "image/gif")),
                            )
                        }),
                    ),
                ),
            ),
        )
        assertEquals(listOf(1L, 2L, 3L, 7L), input.candidates.map { it.messageId })
        val byId = input.candidates.associateBy { it.messageId }
        assertTrue(byId.getValue(1).fromSelf)
        assertTrue(byId.getValue(2).mentionsMe)
        assertTrue(byId.getValue(2).fromContact)
        assertTrue(byId.getValue(3).repliesToMe)
        assertEquals(1L, byId.getValue(3).replyTo)
        assertEquals(2, byId.getValue(7).reactionsDistinct)
        assertTrue(byId.getValue(7).hasMedia)
        assertEquals(input.candidates.size, input.refs.size)
    }

    @Test
    fun channelPostsAreCandidatesWithTheCursorAsIdAndNoReplyOrReactions() {
        val input = buildFeedInput(ctx(emptyList(), posts = listOf(PersistedChannelPost(9, now, "post @anna"))))
        val c = input.candidates.single()
        assertEquals(1, c.conversation)
        assertEquals(9L, c.messageId)
        assertNull(c.replyTo)
        assertEquals(0, c.reactionsDistinct)
        assertFalse(c.fromSelf)
        assertTrue(c.mentionsMe)

        val own = buildFeedInput(
            ctx(emptyList(), posts = listOf(PersistedChannelPost(9, now, "x")), ownChannels = setOf(channel.idHex)),
        )
        assertTrue(own.candidates.single().fromSelf)
        assertEquals("outgoing", own.refs.single().message.direction)
    }

    private fun persistedRef(mime: String, kind: String?, fill: Byte) = me.lakko.app.chat.PersistedAttachment(
        blobIdB64 = java.util.Base64.getEncoder().encodeToString(ByteArray(32) { fill }),
        keyB64 = java.util.Base64.getEncoder().encodeToString(ByteArray(32) { 7 }),
        nonceB64 = java.util.Base64.getEncoder().encodeToString(ByteArray(12) { 9 }),
        mime = mime,
        size = 1234uL,
        name = null,
        kind = kind,
    )

    @Test
    fun channelMediaPostsAreCandidatesWithTheirAttachmentsAsAGroupsMediaMessage() {
        // Base scenario 32 (2026-11-13): a channel's video and voice messages,
        // photo, file and GIF posted without a caption never reached the feed,
        // and a captioned one lost its media.
        val photo = PersistedChannelPostMedia("image/jpeg", null, persistedRef("image/jpeg", null, 1))
        val voice = PersistedChannelPostMedia("audio/mp4", "voice", persistedRef("audio/mp4", "voice", 2))
        val note = PersistedChannelPostMedia("video/mp4", "video_note", persistedRef("video/mp4", "video_note", 3))
        val file = PersistedChannelPostMedia("text/plain", null, persistedRef("text/plain", null, 4))
        val gif = PersistedChannelPostMedia("image/gif", "gif", persistedRef("image/gif", "gif", 5))
        val input = buildFeedInput(
            ctx(
                emptyList(),
                posts = listOf(
                    PersistedChannelPost(1, now, "", listOf(photo)),
                    PersistedChannelPost(2, now, "", listOf(voice)),
                    PersistedChannelPost(3, now, "", listOf(note)),
                    PersistedChannelPost(4, now, "", listOf(file)),
                    PersistedChannelPost(5, now, "", listOf(gif)),
                    PersistedChannelPost(6, now, "caption", listOf(photo)),
                    // Nothing to show: still left out.
                    PersistedChannelPost(7, now, "  "),
                ),
            ),
        )
        assertEquals(listOf(1L, 2L, 3L, 4L, 5L, 6L), input.candidates.map { it.messageId })
        assertTrue(input.candidates.all { it.hasMedia })
        val byId = input.refs.associateBy { it.message.messageId }
        val photoAttachment = byId.getValue(1).message.attachments.single()
        assertEquals("image/jpeg", photoAttachment.mime)
        assertEquals(1234uL, photoAttachment.ref!!.size)
        assertTrue(photoAttachment.ref!!.blobId.all { it == 1.toByte() })
        assertEquals("voice", byId.getValue(2).message.attachments.single().ref!!.kind)
        assertEquals("video_note", byId.getValue(3).message.attachments.single().kind)
        assertEquals("text/plain", byId.getValue(4).message.attachments.single().ref!!.mime)
        assertEquals("gif", byId.getValue(5).message.attachments.single().kind)
        assertEquals("caption", byId.getValue(6).message.text)
        assertEquals(1, byId.getValue(6).message.attachments.size)
    }

    private class FakeCore(val entries: List<FeedEntry>) : FeedCore {
        var lastConversations: List<FeedConversation> = emptyList()
        var lastCandidates: List<FeedCandidate> = emptyList()
        override fun rank(rule: FeedRule, conversations: List<FeedConversation>, candidates: List<FeedCandidate>, nowMs: Long): List<FeedEntry> {
            lastConversations = conversations
            lastCandidates = candidates
            return entries
        }
        override fun mentionsUsername(text: String, username: String) = false
        override fun contiguousSeen(ids: List<Long>, seen: List<Long>): Long? = null
        override fun validate(rule: FeedRule) = Unit
        override fun encode(rule: FeedRule) = ByteArray(0)
        override fun decode(bytes: ByteArray): FeedRule = error("unused")
        override fun export(rule: FeedRule): FeedRule = rule
        override fun preset(preset: FeedPreset): FeedRule = error("unused")
        override fun normalizeWord(raw: String): String? = raw.trim().lowercase().ifEmpty { null }
        override fun maxWeight() = 100
        override fun ruleMaxLen() = 4096
    }

    @Test
    fun rankingMapsIndicesBackAndIgnoresOutOfRangeOnes() {
        val input = buildFeedInput(ctx(listOf(msg(1, "incoming"), msg(2, "incoming", extra = arrayOf({ it.copy(replyTarget = 1L) })))))
        val core = FakeCore(
            listOf(
                FeedEntry(index = 0u, replies = listOf(1u, 99u)),
                FeedEntry(index = 42u, replies = emptyList()),
            ),
        )
        val rule = FeedRule(
            "x", 0u, true, true, emptyList(), emptyList(), false, false,
            uniffi.lakko_ffi_uniffi.FeedWeights(100u, 0u, 0u, 0u, 0u, 0u),
            uniffi.lakko_ffi_uniffi.FeedHalfLife.DAY, 30u, false, false, false, 0u, emptyList(), emptyList(),
            uniffi.lakko_ffi_uniffi.FeedSuggestions.NEVER,
        )
        val ranked = rankFeedInput(core, rule, input, now)
        assertEquals(1, ranked.size)
        assertEquals(1L, ranked[0].ref.message.messageId)
        assertEquals(listOf(2L), ranked[0].replies.map { it.message.messageId })
        assertEquals(32, core.lastConversations[0].id.size)
        assertEquals(16, core.lastConversations[1].id.size)
    }

    @Test
    fun reactionSummaryCountsMostFirst() {
        val m = msg(1, "incoming", extra = arrayOf({
            it.copy(reactions = listOf(FeedReaction("a", "x"), FeedReaction("b", "y"), FeedReaction("c", "y")))
        }))
        assertEquals(listOf(ReactionCount("y", 2), ReactionCount("x", 1)), reactionSummary(m))
    }
}
