// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.push.isServiceEnvelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage

/**
 * ADR-0048 trigger 4 — the "introduce yourselves" request is a SERVICE
 * envelope on both receive paths: the foreground classifier turns it into a
 * `ProfileRequest` event bound to the sender key resolved at receipt (never a
 * chat bubble, even when malformed), and the background push filter
 * suppresses it.
 *
 * ## Fixture hygiene
 *
 * The domain separator ends in a NUL byte, appended programmatically — a raw
 * NUL in a source file makes git treat it as binary.
 *
 * ## The doubles
 *
 * [lossyParseBody] models the real `parseBodyEnvelope` contract: unrecognised
 * bytes DEGRADE to a chat body with the lossy-UTF-8 text rather than throwing,
 * which is exactly how an unfiltered request would reach the timeline as the
 * text `lakko/profile-request/v1`. [fakeRequestDecode] stands in for the
 * native `decodeProfileRequest` with the codec's three gates (domain, exact
 * length, version) so the arm is not passing merely because the double says
 * yes to everything.
 */
class ProfileRequestClassificationTest {

    private val requestDomain: ByteArray =
        "lakko/profile-request/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

    /** A well-formed request: `domain(25) || version(1)=0x01`. Field-less. */
    private fun request(version: Int = 0x01): ByteArray = requestDomain + byteArrayOf(version.toByte())

    private fun fakeRequestDecode(bytes: ByteArray): Boolean =
        bytes.size == requestDomain.size + 1 &&
            bytes.copyOfRange(0, requestDomain.size).contentEquals(requestDomain) &&
            bytes[requestDomain.size].toInt() == 0x01

    private fun lossyParseBody(bytes: ByteArray): ParsedBody = ParsedBody(
        kind = MessageBodyKind.CHAT,
        text = String(bytes, Charsets.UTF_8),
        attachments = emptyList(),
        target = null,
        emoji = null,
        isTyping = null,
        // ADR-0052 - a reply is a `CHAT` that also points somewhere; nothing here is one.
        replyTo = null,
        replyAuthor = null,
        replyExcerpt = null,
    )

    private fun classify(
        payload: ByteArray,
        resolveSender: (UInt) -> MemberIdentity? = { leaf -> "KEY_OF_LEAF_$leaf" },
        senderLeaf: UInt? = 3u,
        isProfileRequest: (ByteArray) -> Boolean = ::fakeRequestDecode,
    ): IncomingChatEvent = toEvent(
        messageId = 7L,
        createdAt = "2026-09-19T00:00:00Z",
        processed = ProcessedMessage.Application(payload, senderLeaf, null),
        resolveSender = resolveSender,
        resolveLeafSignatureKey = { null },
        isCommitterBeacon = { false },
        parseBody = ::lossyParseBody,
        decodeProfile = { null },
        decodeGroupName = { null },
        isProfileRequest = isProfileRequest,
    )

    @Test
    fun `the decode double rejects what the codec rejects`() {
        assertTrue(fakeRequestDecode(request()))
        assertFalse(fakeRequestDecode(requestDomain))
        assertFalse(fakeRequestDecode(request() + byteArrayOf(0)))
        assertFalse(fakeRequestDecode(request(version = 0x02)))
    }

    @Test
    fun `a request classifies as a request bound to the resolved sender key`() {
        val event = classify(request())
        assertTrue("expected ProfileRequest, got $event", event is IncomingChatEvent.ProfileRequest)
        event as IncomingChatEvent.ProfileRequest
        assertTrue(event.wellFormed)
        assertEquals("KEY_OF_LEAF_3", event.senderKey)
        assertEquals(7L, event.messageId)
    }

    @Test
    fun `a request never reaches the timeline as text`() {
        // Written against the rendered text rather than the variant, so it
        // keeps discriminating if the arm is ever re-routed.
        val rendered = (classify(request()) as? IncomingChatEvent.Application)?.text ?: ""
        assertFalse(
            "the request rendered as a chat bubble reading '$rendered'",
            rendered.contains("profile-request"),
        )
    }

    @Test
    fun `a malformed request is dropped, never rendered and never answered`() {
        // An unknown version: the peek classifies it, the codec refuses it.
        val event = classify(request(version = 0x02))
        assertTrue("expected ProfileRequest(wellFormed=false), got $event", event is IncomingChatEvent.ProfileRequest)
        assertFalse((event as IncomingChatEvent.ProfileRequest).wellFormed)
        // The bare separator too.
        val bare = classify(requestDomain)
        assertTrue(bare is IncomingChatEvent.ProfileRequest)
        assertFalse((bare as IncomingChatEvent.ProfileRequest).wellFormed)
    }

    @Test
    fun `an unattributable sender yields no key to answer`() {
        val event = classify(request(), senderLeaf = null)
        assertNull((event as IncomingChatEvent.ProfileRequest).senderKey)
    }

    @Test
    fun `a chat envelope is untouched by the request arm`() {
        var decodeCalls = 0
        val chat = """{"v":1,"text":"lakko/profile-request/v1","attachments":[]}""".toByteArray(Charsets.UTF_8)
        val event = classify(chat, isProfileRequest = { decodeCalls++; true })
        assertTrue("expected Application, got $event", event is IncomingChatEvent.Application)
        assertEquals(0, decodeCalls)
    }

    @Test
    fun `a legacy plain text that starts with the ASCII domain is still a message`() {
        // Without the NUL it is not the separator: a person could type this.
        val legacy = "lakko/profile-request/v1 is a fine subject".toByteArray(Charsets.UTF_8)
        val event = classify(legacy, isProfileRequest = { true })
        assertTrue("expected Application, got $event", event is IncomingChatEvent.Application)
    }

    @Test
    fun `the background push filter suppresses a request, well-formed or not`() {
        val service: (ByteArray) -> Boolean = { payload ->
            isServiceEnvelope(
                payload,
                decodesAsRoutingSecret = { false },
                decodesAsCommitterBeacon = { false },
                committerBeaconEnvelopeLen = 27,
            )
        }
        assertTrue(service(request()))
        assertTrue(service(request(version = 0x02)))
        assertTrue(service(requestDomain))
        assertFalse(service("""{"v":1,"text":"lakko/profile-request/v1","attachments":[]}""".toByteArray()))
        assertFalse(service("lakko/profile-request/v1 is a fine subject".toByteArray()))
    }
}
