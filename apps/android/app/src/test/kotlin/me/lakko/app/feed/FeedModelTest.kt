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

/** ADR-0058 — assembling the ranking's input and mapping its answer back. */
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
