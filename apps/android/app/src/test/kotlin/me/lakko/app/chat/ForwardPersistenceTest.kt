// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * ADR-0052 decision 2 — a forward's attribution survives a relaunch, and a
 * row written before forwards existed reads back as an ordinary message.
 *
 * Pure JSON work, like [ReplyPersistenceTest]: no Keystore, no `Context`.
 */
class ForwardPersistenceTest {
    private val json = Json { ignoreUnknownKeys = true }

    private fun forwarded(forward: ForwardRef?, deleted: Boolean = false) = ChatMessage(
        messageId = 9L,
        direction = Direction.Outgoing,
        text = "passed along",
        createdAt = "1970-01-01T00:00:00Z",
        authorKey = "bWU=",
        deleted = deleted,
        forward = forward,
    )

    private fun roundTrip(msg: ChatMessage): ChatMessage? {
        val blob = json.encodeToString(PersistedMessage.serializer(), PersistedMessage.from(msg))
        return json.decodeFromString(PersistedMessage.serializer(), blob).toChatMessage()
    }

    @Test
    fun `the attribution round-trips through the log`() {
        val ref = ForwardRef(author = "Anna", originGroupIdHex = "ab".repeat(16), originMessageId = 4L)
        val restored = roundTrip(forwarded(ref))
        assertNotNull(restored)
        assertEquals(ref, restored!!.forward)
    }

    @Test
    fun `a nameless forward with an origin keeps the origin`() {
        val ref = ForwardRef(author = null, originGroupIdHex = "ab".repeat(16), originMessageId = 4L)
        assertEquals(ref, roundTrip(forwarded(ref))!!.forward)
    }

    @Test
    fun `an origin-less forward keeps the name`() {
        val ref = ForwardRef(author = "Anna", originGroupIdHex = null, originMessageId = null)
        assertEquals(ref, roundTrip(forwarded(ref))!!.forward)
    }

    @Test
    fun `a row stored without the field reads as an ordinary message`() {
        // The shape a transcript written before this field existed has on disk.
        val legacy = """{"messageId":9,"direction":"outgoing","text":"hi","attachments":[],""" +
            """"createdAt":"1970-01-01T00:00:00Z"}"""
        val restored = json.decodeFromString(PersistedMessage.serializer(), legacy).toChatMessage()
        assertNotNull(restored)
        assertNull(restored!!.forward)
    }

    @Test
    fun `a deleted forward drops its attribution with its words`() {
        val ref = ForwardRef(author = "Anna", originGroupIdHex = "ab".repeat(16), originMessageId = 4L)
        val stored = PersistedMessage.from(forwarded(ref, deleted = true))
        assertNull(stored.forward)
        assertEquals("", stored.text)
    }
}
