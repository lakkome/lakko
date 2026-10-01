// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.serialization.json.Json
import me.lakko.app.push.ProcessedKind
import me.lakko.app.push.previewKindOf
import me.lakko.app.ui.chat.UNSUPPORTED_MESSAGE_KEY
import me.lakko.app.ui.chat.previewLine
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage

/**
 * ADR-0061 section 6 — a chat envelope from a newer wire version arrives from
 * the core as [MessageBodyKind.UNSUPPORTED] with no text. It is a timeline row
 * (a real message the person should know exists), it survives a relaunch, and
 * every surface renders the catalog sentence `chat.message.unsupported` —
 * never the core's English "[unsupported vN message]".
 */
class UnsupportedMessageTest {

    private fun unsupportedBody(version: Long): ParsedBody = ParsedBody(
        kind = MessageBodyKind.UNSUPPORTED,
        text = "",
        attachments = emptyList(),
        target = version,
        emoji = null,
        isTyping = null,
        replyTo = null,
        replyAuthor = null,
        replyExcerpt = null,
        forwardedFrom = null,
        forwardOriginGroupB64 = null,
        forwardOriginMessageId = null,
    )

    private fun classify(body: ParsedBody): IncomingChatEvent = toEvent(
        messageId = 5L,
        createdAt = "2026-10-01T00:00:00Z",
        processed = ProcessedMessage.Application("{\"v\":9}".toByteArray(), 3u, null),
        resolveSender = { null },
        resolveLeafSignatureKey = { null },
        isCommitterBeacon = { false },
        parseBody = { body },
    )

    @Test
    fun `a newer-version envelope is a timeline row carrying its version and no text`() {
        val event = classify(unsupportedBody(2L))
        assertTrue(event is IncomingChatEvent.Application)
        event as IncomingChatEvent.Application
        assertEquals(2L, event.unsupportedVersion)
        assertEquals("", event.text)
        assertTrue(event.attachments.isEmpty())
    }

    @Test
    fun `the version survives the message log`() {
        val json = Json { ignoreUnknownKeys = true }
        val msg = ChatMessage(
            messageId = 5L,
            direction = Direction.Incoming,
            text = "",
            createdAt = "2026-10-01T00:00:00Z",
            unsupportedVersion = 2L,
        )
        val blob = json.encodeToString(PersistedMessage.serializer(), PersistedMessage.from(msg))
        val restored = json.decodeFromString(PersistedMessage.serializer(), blob).toChatMessage()
        assertEquals(2L, restored!!.unsupportedVersion)
    }

    @Test
    fun `a row stored before the field existed is an ordinary message`() {
        val json = Json { ignoreUnknownKeys = true }
        val legacy = """{"messageId":9,"direction":"incoming","text":"hi","attachments":[],""" +
            """"createdAt":"1970-01-01T00:00:00Z"}"""
        assertNull(json.decodeFromString(PersistedMessage.serializer(), legacy).toChatMessage()!!.unsupportedVersion)
    }

    @Test
    fun `the conversation row reads the catalog sentence`() {
        val line = previewLine(
            text = "",
            attachments = emptyList(),
            deleted = false,
            author = null,
            unsupported = true,
        )
        assertEquals("This message needs a newer version of Lakko.", line)
        assertEquals("chat.message.unsupported", UNSUPPORTED_MESSAGE_KEY)
        assertFalse(line.contains("unsupported v"))
    }

    @Test
    fun `the background preview maps it to its own kind`() {
        assertEquals(ProcessedKind.UNSUPPORTED, previewKindOf(MessageBodyKind.UNSUPPORTED))
    }
}
