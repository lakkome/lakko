// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import me.lakko.app.chat.ChatMessage
import me.lakko.app.chat.Direction
import me.lakko.app.chat.IncomingChatEvent
import me.lakko.app.chat.PersistedMessage
import me.lakko.app.chat.from
import me.lakko.app.chat.toChatMessage
import me.lakko.app.chat.toEvent
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage

/** ADR-0099 section 1 — attested chat messages on the receive path and in the store. */
class AttestationTest {

    private val envelope = """{"v":1,"text":"hello"}""".toByteArray()
    private val attested = ATTESTED_DOMAIN + byteArrayOf(1) + ByteArray(64) + envelope

    @Test
    fun `the peek recognises the wrapper's domain and nothing else`() {
        assertTrue(looksAttested(attested))
        assertFalse(looksAttested(envelope))
        assertFalse(looksAttested("lakko/profile/v1".toByteArray() + byteArrayOf(0)))
        assertFalse(looksAttested(ATTESTED_DOMAIN.copyOf(ATTESTED_DOMAIN.size - 1)))
        assertEquals(26, ATTESTED_DOMAIN.size)
    }

    @Test
    fun `a plain plaintext never reaches the core's unwrap`() {
        var called = false
        val out = unwrapForBody(envelope) {
            called = true
            it
        }
        assertArrayEquals(envelope, out)
        assertFalse(called)
        assertArrayEquals(envelope, unwrapForBody(attested) { it.copyOfRange(ATTESTED_OVERHEAD, it.size) })
    }

    private fun parse(bytes: ByteArray): ParsedBody = ParsedBody(
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

    @Test
    fun `the body parser reads the envelope inside the wrapper and the row keeps its evidence`() {
        val evidence = MessageEvidence(AttestationVerdict.VERIFIED, "cA==", "aw==", "Yg==")
        var seenPlaintext: ByteArray? = null
        var seenLeaf: UInt? = null
        val event = toEvent(
            messageId = 4L,
            createdAt = "2026-11-08T00:00:00Z",
            processed = ProcessedMessage.Application(attested, 2u, null),
            resolveSender = { "KEY_$it" },
            resolveLeafSignatureKey = { null },
            isCommitterBeacon = { false },
            parseBody = ::parse,
            decodeProfile = { null },
            decodeGroupName = { null },
            decodeGroupClosed = { null },
            isProfileRequest = { false },
            decodePin = { null },
            decodeSharedChannels = { null },
            isRegistrarSeed = { false },
            unwrapBody = { it.copyOfRange(ATTESTED_OVERHEAD, it.size) },
            evidenceOf = { plaintext, leaf ->
                seenPlaintext = plaintext
                seenLeaf = leaf
                evidence
            },
        )
        assertTrue("got $event", event is IncomingChatEvent.Application)
        event as IncomingChatEvent.Application
        assertEquals("""{"v":1,"text":"hello"}""", event.text)
        assertEquals(evidence, event.evidence)
        // The evidence is the plaintext as received, wrapper included.
        assertArrayEquals(attested, seenPlaintext)
        assertEquals(2u, seenLeaf)
    }

    @Test
    fun `the evidence is stored with the row and dropped with a deleted row`() {
        val evidence = MessageEvidence(AttestationVerdict.FAILED, "cA==", "aw==", null)
        val msg = ChatMessage(
            messageId = 9,
            direction = Direction.Incoming,
            text = "hi",
            createdAt = "2026-11-08T00:00:00Z",
            evidence = evidence,
        )
        assertEquals(evidence, PersistedMessage.from(msg).toChatMessage()?.evidence)
        assertNull(PersistedMessage.from(msg.copy(deleted = true)).evidence)
    }

    @Test
    fun `evidence bytes decode back to what was captured`() {
        val e = MessageEvidence.of(AttestationVerdict.VERIFIED, byteArrayOf(1, 2), byteArrayOf(3), null)
        assertArrayEquals(byteArrayOf(1, 2), e.plaintext)
        assertArrayEquals(byteArrayOf(3), e.leafKey)
        assertArrayEquals(ByteArray(0), e.binding)
        assertEquals(AttestationVerdict.ABSENT, AttestationVerdict.fromWire("absent"))
    }

    // ADR-0099 F10 — attesting is the default; the sends that must not attest say so.

    private fun source(path: String): String = java.io.File("src/main/kotlin/me/lakko/app/$path").readText()

    @Test
    fun `an ordinary chat send is attested by default`() {
        val client = source("chat/ChatClient.kt")
        val send = client.substringAfter("    public suspend fun sendMessage(").substringBefore("): SendMessageResult")
        assertTrue("attest: Boolean = true" in send)
        val prepare = client.substringAfter("    public suspend fun prepareSend(").substringBefore("): PreparedSend")
        assertTrue("attest: Boolean = true" in prepare)
        assertFalse("attest: Boolean = false" in client)
    }

    @Test
    fun `every send names whether it attests and only the non-chat ones do not`() {
        val repo = source("chat/ChatRepository.kt")
        val calls = Regex("""chatClient\.prepareSend\((.*?)\n\s*\)""", RegexOption.DOT_MATCHES_ALL)
            .findAll(repo).map { it.groupValues[1] }.toList()
        assertTrue(calls.isNotEmpty())
        for (call in calls) {
            assertTrue(call, "attest = true" in call || "attest = false" in call)
        }
        fun bodyOf(fn: String) = repo.substringAfter("fun $fn(").substringBefore("\n    }\n")
        // Typing, read receipts, call signaling and control envelopes are never attested.
        for (fn in listOf(
            "sendTypingOnMlsThread",
            "sendReadReceiptOnMlsThread",
            "publishApplicationMessageOnMlsThread",
            "broadcastControlToGroup",
        )) {
            val body = bodyOf(fn)
            assertTrue(fn, "chatClient.prepareSend(" in body && "attest = false" in body)
            assertFalse(fn, "attest = true" in body)
        }
        assertEquals(4, Regex("attest = false").findAll(repo).count())
    }
}
