// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.calls.CallOutcome
import me.lakko.app.calls.CallTranscriptEntry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * `docs/ux/calls.md` section 6 — "make it survive a relaunch the way the
 * timeline does".
 *
 * The call row rides the existing encrypted message log rather than a store of
 * its own, so what has to be true is that it round-trips through
 * [PersistedMessage] unchanged, and that a transcript written before call rows
 * existed still decodes. Both are pure JSON work, which is why they can be
 * tested here with no Keystore and no `Context`.
 */
class CallRowPersistenceTest {
    init {
        // ADR-0061: the words under test are the English catalog's.
        EnglishForTests.install()
    }


    private fun row(entry: CallTranscriptEntry) = ChatMessage(
        messageId = -1L,
        direction = Direction.System,
        text = me.lakko.app.calls.callRowText(entry),
        createdAt = "2026-09-17T12:04:00Z",
        call = entry,
    )

    @Test
    fun `a finished call round-trips through the transcript`() {
        val entry = CallTranscriptEntry(CallOutcome.OUTGOING, video = true, durationSecs = 724)
        val restored = PersistedMessage.from(row(entry)).toChatMessage()
        assertNotNull(restored)
        assertEquals(entry, restored!!.call)
        assertEquals(Direction.System, restored.direction)
        assertEquals("Outgoing call, 12:04", restored.text)
        assertEquals(-1L, restored.messageId)
    }

    @Test
    fun `every outcome survives the round trip`() {
        for (outcome in CallOutcome.entries) {
            val entry = CallTranscriptEntry(outcome, video = false, durationSecs = null)
            val restored = PersistedMessage.from(row(entry)).toChatMessage()
            assertEquals(outcome.name, entry, restored?.call)
        }
    }

    @Test
    fun `an ordinary message carries no call marker`() {
        val message = ChatMessage(
            messageId = 7L,
            direction = Direction.Incoming,
            text = "hello",
            createdAt = "2026-09-17T12:04:00Z",
        )
        val persisted = PersistedMessage.from(message)
        assertNull(persisted.call)
        assertNull(persisted.toChatMessage()?.call)
    }

    @Test
    fun `a transcript written before call rows existed still decodes`() {
        // The key is simply absent — nothing to migrate, nothing to wipe.
        val json = """
            {"version":1,"groups":{"aa":{"cursor":9,"messages":[
              {"messageId":1,"direction":"incoming","text":"hi","attachments":[],
               "createdAt":"2026-09-17T12:00:00Z"}
            ]}}}
        """.trimIndent()
        val payload = MessageLogStore.decodeLogPayload(json)
        assertNotNull(payload)
        val restored = payload!!.groups.getValue("aa").messages.single().toChatMessage()
        assertNotNull(restored)
        assertNull(restored!!.call)
        assertEquals("hi", restored.text)
    }

    @Test
    fun `an outcome this build does not know leaves the row standing`() {
        // Fail-soft in the direction that keeps the transcript intact: the
        // marker is dropped, the row and its stored words remain and render as
        // the plain system pill they already are.
        val persisted = PersistedMessage(
            messageId = -2L,
            direction = "system",
            text = "Forwarded call",
            attachments = emptyList(),
            createdAt = "2026-09-17T12:04:00Z",
            call = PersistedCall(outcome = "forwarded", video = false),
        )
        val restored = persisted.toChatMessage()
        assertNotNull(restored)
        assertNull(restored!!.call)
        assertEquals("Forwarded call", restored.text)
    }

    @Test
    fun `a stored negative duration is not restored as one`() {
        val persisted = PersistedMessage(
            messageId = -3L,
            direction = "system",
            text = "Outgoing call",
            attachments = emptyList(),
            createdAt = "2026-09-17T12:04:00Z",
            call = PersistedCall(outcome = "outgoing", video = false, durationSecs = -5L),
        )
        assertNull(persisted.toChatMessage()?.call?.durationSecs)
    }

    @Test
    fun `a call row never joins a visual run`() {
        // It is a system row, so every rule that isolates system rows applies
        // to it unchanged — it is an event, not something somebody said.
        val entry = CallTranscriptEntry(CallOutcome.INCOMING, video = false, durationSecs = 30)
        val call = row(entry)
        val message = ChatMessage(
            messageId = 4L,
            direction = Direction.Incoming,
            text = "hi",
            createdAt = "2026-09-17T12:04:00Z",
        )
        val zone = java.time.ZoneId.of("UTC")
        assertEquals(
            false,
            me.lakko.app.ui.chat.continuesRun(message, 1_000L, call, 2_000L, zone),
        )
        assertEquals(
            false,
            me.lakko.app.ui.chat.continuesRun(call, 1_000L, message, 2_000L, zone),
        )
    }
}
