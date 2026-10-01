// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.push.BackgroundPayloadKind
import me.lakko.app.push.ProcessedKind
import me.lakko.app.push.classifyBackgroundPayload
import me.lakko.app.push.previewKindOf
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage

/**
 * A `lakko/`-domain envelope this build cannot decode is never rendered.
 *
 * Found live on test.lakko.me: a client on another build sent a profile
 * envelope v3 into shared groups, and main clients showed a bubble reading
 * `lakko/profile/v1` plus binary. The core now answers
 * [MessageBodyKind.SERVICE_ENVELOPE] for any non-JSON plaintext that opens with
 * `lakko/`; the foreground classifier turns it into
 * [IncomingChatEvent.ServiceEnvelope] (no timeline row, hence no chat-list
 * preview), and the background preview maps it to "no preview".
 *
 * The parse double models the core's CURRENT contract: JSON is chat, a
 * `lakko/` prefix is a service envelope, anything else degrades to lossy chat
 * text. It never throws, like the real parser. Every NUL is appended
 * programmatically so this file stays text to git.
 */
class ServiceEnvelopeKindTest {

    private val profileDomain: ByteArray =
        "lakko/profile/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

    /** The shape seen live: the v1 profile domain with a version byte 3 and
     *  a body this build does not know. */
    private fun profileV3(): ByteArray =
        profileDomain + byteArrayOf(0x03, 0x00, 0x01, 0x7F, 0x05) + "Alice".toByteArray()

    /** An envelope kind this build has never heard of. */
    private fun unknownEnvelope(): ByteArray =
        "lakko/folder/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0, 0x01, 0x02, 0x03)

    private fun parsed(kind: MessageBodyKind, text: String): ParsedBody = ParsedBody(
        kind = kind,
        text = text,
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

    private fun coreParseBody(bytes: ByteArray): ParsedBody {
        val lakko = "lakko/".toByteArray(Charsets.US_ASCII)
        return when {
            bytes.isNotEmpty() && bytes[0] == '{'.code.toByte() ->
                parsed(MessageBodyKind.CHAT, "json")
            bytes.size >= lakko.size && bytes.copyOfRange(0, lakko.size).contentEquals(lakko) ->
                parsed(MessageBodyKind.SERVICE_ENVELOPE, "")
            else -> parsed(MessageBodyKind.CHAT, String(bytes, Charsets.UTF_8))
        }
    }

    private fun classify(payload: ByteArray) =
        toEvent(
            messageId = 11L,
            createdAt = "2026-09-29T00:00:00Z",
            processed = ProcessedMessage.Application(payload, 2u, null),
            resolveSender = { leaf -> "KEY_OF_LEAF_$leaf" },
            resolveLeafSignatureKey = { null },
            isCommitterBeacon = { false },
            parseBody = ::coreParseBody,
            decodeProfile = { null },
            decodeGroupName = { null },
            isProfileRequest = { false },
            decodePin = { null },
            isRegistrarSeed = { false },
        )

    private fun renderedText(event: IncomingChatEvent): String? =
        (event as? IncomingChatEvent.Application)?.text

    @Test
    fun `a v3 profile envelope never becomes a bubble`() {
        val event = classify(profileV3())
        assertNull("rendered as '${renderedText(event)}'", renderedText(event))
        // The profile arm catches it by domain and drops it undecoded.
        assertTrue("expected Profile(null), got $event", event is IncomingChatEvent.Profile)
        assertNull((event as IncomingChatEvent.Profile).profile)
    }

    @Test
    fun `an unknown lakko envelope is a service envelope, never a bubble`() {
        val event = classify(unknownEnvelope())
        assertNull("rendered as '${renderedText(event)}'", renderedText(event))
        assertTrue("expected ServiceEnvelope, got $event", event is IncomingChatEvent.ServiceEnvelope)
        assertEquals(11L, (event as IncomingChatEvent.ServiceEnvelope).messageId)
    }

    @Test
    fun `a chat envelope and legacy text still render`() {
        assertTrue(classify("""{"v":1,"text":"hi"}""".toByteArray()) is IncomingChatEvent.Application)
        assertEquals("plain words", renderedText(classify("plain words".toByteArray())))
    }

    @Test
    fun `the background filter drops a v3 profile before the parser`() {
        assertEquals(
            BackgroundPayloadKind.SERVICE_ENVELOPE,
            classifyBackgroundPayload(
                profileV3(),
                decodesAsRoutingSecret = { false },
                decodesAsCommitterBeacon = { false },
                committerBeaconEnvelopeLen = 27,
            ),
        )
    }

    @Test
    fun `the background preview has no kind for a service envelope`() {
        assertNull(previewKindOf(MessageBodyKind.SERVICE_ENVELOPE))
        assertEquals(ProcessedKind.CHAT, previewKindOf(MessageBodyKind.CHAT))
        assertEquals(ProcessedKind.READ_RECEIPT, previewKindOf(MessageBodyKind.READ_RECEIPT))
        // An unknown envelope passes the background peeks (none knows its
        // domain) and is caught by the parser's kind instead.
        assertEquals(
            BackgroundPayloadKind.MESSAGE,
            classifyBackgroundPayload(
                unknownEnvelope(),
                decodesAsRoutingSecret = { false },
                decodesAsCommitterBeacon = { false },
                committerBeaconEnvelopeLen = 27,
            ),
        )
        assertNull(previewKindOf(coreParseBody(unknownEnvelope()).kind))
        assertFalse(previewKindOf(coreParseBody("hello".toByteArray()).kind) == null)
    }
}
