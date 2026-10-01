// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.push.BackgroundPayloadKind
import me.lakko.app.push.classifyBackgroundPayload
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage

/**
 * ADR-0057 obligation 4 — a registrar-seed envelope is a SERVICE envelope on
 * both receive paths: the foreground classifier turns it into a
 * `RegistrarSeed` event bound to the sender key resolved at receipt (never a
 * chat bubble), and the background push filter suppresses it.
 *
 * The envelope length is the core's `registrarSeedEnvelopeLen()` (173); the
 * double below stands in for the native decode with the length and the first
 * byte, so the arm is not passing merely because the double says yes.
 */
class RegistrarSeedClassificationTest {

    private val len = 173

    private fun seedEnvelope(): ByteArray = ByteArray(len) { if (it == 0) 0x6c else 0x11 }

    private fun fakeDecode(bytes: ByteArray): Boolean = bytes.size == len && bytes[0].toInt() == 0x6c

    private fun lossyParseBody(bytes: ByteArray): ParsedBody = ParsedBody(
        kind = MessageBodyKind.CHAT,
        text = String(bytes, Charsets.UTF_8),
        attachments = emptyList(),
        target = null,
        emoji = null,
        isTyping = null,
        replyTo = null,
        replyAuthor = null,
        replyExcerpt = null,
        forwardedFrom = null,
        forwardOriginGroupB64 = null,
        forwardOriginMessageId = null,
    )

    private fun classify(payload: ByteArray, senderLeaf: UInt? = 2u): IncomingChatEvent = toEvent(
        messageId = 9L,
        createdAt = "2026-09-28T00:00:00Z",
        processed = ProcessedMessage.Application(payload, senderLeaf, null),
        resolveSender = { leaf -> "ACCOUNT_OF_LEAF_$leaf" },
        resolveLeafSignatureKey = { null },
        isCommitterBeacon = { false },
        parseBody = ::lossyParseBody,
        decodeProfile = { null },
        decodeGroupName = { null },
        isProfileRequest = { false },
        decodePin = { null },
        isRegistrarSeed = { bytes -> looksLikeRegistrarSeedEnvelope(bytes, len, ::fakeDecode) },
    )

    @Test
    fun `a registrar seed classifies as its own event bound to the resolved sender`() {
        val envelope = seedEnvelope()
        val event = classify(envelope)
        assertTrue("expected RegistrarSeed, got $event", event is IncomingChatEvent.RegistrarSeed)
        event as IncomingChatEvent.RegistrarSeed
        assertEquals("ACCOUNT_OF_LEAF_2", event.senderKey)
        assertArrayEquals(envelope, event.envelope)
        assertEquals(9L, event.messageId)
    }

    @Test
    fun `a registrar seed never reaches the timeline`() {
        assertFalse(classify(seedEnvelope()) is IncomingChatEvent.Application)
    }

    @Test
    fun `the peek needs the exact length, the first byte and the decode`() {
        assertTrue(looksLikeRegistrarSeedEnvelope(seedEnvelope(), len, ::fakeDecode))
        assertFalse(looksLikeRegistrarSeedEnvelope(seedEnvelope().copyOf(len - 1), len, ::fakeDecode))
        assertFalse(looksLikeRegistrarSeedEnvelope(ByteArray(len) { 0x7b }, len, ::fakeDecode))
        assertFalse(looksLikeRegistrarSeedEnvelope(seedEnvelope(), len) { false })
        assertFalse(looksLikeRegistrarSeedEnvelope(seedEnvelope(), -1, ::fakeDecode))
    }

    @Test
    fun `a chat envelope is untouched by the registrar arm`() {
        val chat = """{"v":1,"text":"hello","attachments":[]}""".toByteArray(Charsets.UTF_8)
        assertTrue(classify(chat) is IncomingChatEvent.Application)
    }

    @Test
    fun `the background filter suppresses a registrar seed`() {
        val kind = classifyBackgroundPayload(
            seedEnvelope(),
            decodesAsRoutingSecret = { false },
            decodesAsCommitterBeacon = { false },
            committerBeaconEnvelopeLen = 27,
            decodesAsRegistrarSeed = ::fakeDecode,
            registrarSeedEnvelopeLen = len,
        )
        assertEquals(BackgroundPayloadKind.SERVICE_ENVELOPE, kind)
    }

    @Test
    fun `the background filter still passes a real message`() {
        val kind = classifyBackgroundPayload(
            """{"v":1,"text":"hi","attachments":[]}""".toByteArray(Charsets.UTF_8),
            decodesAsRoutingSecret = { false },
            decodesAsCommitterBeacon = { false },
            committerBeaconEnvelopeLen = 27,
            decodesAsRegistrarSeed = ::fakeDecode,
            registrarSeedEnvelopeLen = len,
        )
        assertEquals(BackgroundPayloadKind.MESSAGE, kind)
    }
}
