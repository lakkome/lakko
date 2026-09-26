// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * ADR-0052 — a reply's quote survives a relaunch, and a row written before
 * replies existed reads back as an ordinary message.
 *
 * Pure JSON work, like [DeletedMessagePersistenceTest]: no Keystore, no
 * `Context`.
 */
class ReplyPersistenceTest {
    private val json = Json { ignoreUnknownKeys = true }

    private fun reply(reply: ReplyRef?, deleted: Boolean = false) = ChatMessage(
        messageId = 9L,
        direction = Direction.Outgoing,
        text = "answering",
        createdAt = "1970-01-01T00:00:00Z",
        authorKey = "bWU=",
        deleted = deleted,
        reply = reply,
    )

    @Test
    fun `the reply round-trips through the log`() {
        val ref = ReplyRef(target = 4L, author = "Anna", excerpt = "the question")
        val blob = json.encodeToString(PersistedMessage.serializer(), PersistedMessage.from(reply(ref)))
        val restored = json.decodeFromString(PersistedMessage.serializer(), blob).toChatMessage()
        assertNotNull(restored)
        assertEquals(ref, restored!!.reply)
    }

    @Test
    fun `a snapshot-less reply keeps its target and nothing else`() {
        val ref = ReplyRef(target = 4L, author = null, excerpt = null)
        val blob = json.encodeToString(PersistedMessage.serializer(), PersistedMessage.from(reply(ref)))
        val restored = json.decodeFromString(PersistedMessage.serializer(), blob).toChatMessage()
        assertEquals(ref, restored!!.reply)
    }

    @Test
    fun `a row stored without the field reads as an ordinary message`() {
        // The shape a transcript written before ADR-0052 has on disk.
        val legacy = """{"messageId":9,"direction":"outgoing","text":"hi","attachments":[],""" +
            """"createdAt":"1970-01-01T00:00:00Z"}"""
        val restored = json.decodeFromString(PersistedMessage.serializer(), legacy).toChatMessage()
        assertNotNull(restored)
        assertNull(restored!!.reply)
    }

    @Test
    fun `a deleted reply drops its quote with its words`() {
        val ref = ReplyRef(target = 4L, author = "Anna", excerpt = "the question")
        val stored = PersistedMessage.from(reply(ref, deleted = true))
        assertNull(stored.reply)
        assertEquals("", stored.text)
    }
}
