// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.push.isServiceEnvelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiProfile
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.ParsedBody
import uniffi.lakko_ffi_uniffi.ProcessedMessage

/**
 * ADR-0048 — a profile envelope is a SERVICE envelope on both receive paths:
 * the foreground classifier turns it into a `Profile` event bound to the
 * sender key resolved at receipt (never a chat bubble, even when malformed),
 * and the background push filter suppresses it.
 *
 * ## Fixture hygiene
 *
 * The domain separator ends in a NUL byte, appended programmatically — a raw
 * NUL in a source file makes git treat it as binary.
 *
 * ## The parse double
 *
 * [lossyParseBody] models the real `parseBodyEnvelope` contract: unrecognised
 * bytes DEGRADE to a chat body with the lossy-UTF-8 text rather than throwing,
 * which is exactly how an unfiltered profile would reach the timeline.
 */
class ProfileEnvelopeClassificationTest {

    private val profileDomain: ByteArray =
        "lakko/profile/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

    /** `domain || version || updated_at_ms(8) || name_len || name || user_len || user`. */
    private fun profileEnvelope(name: String = "Alice", user: String = "alice"): ByteArray {
        val n = name.toByteArray(Charsets.UTF_8)
        val u = user.toByteArray(Charsets.US_ASCII)
        return profileDomain + byteArrayOf(0x01) + ByteArray(8) { if (it == 7) 42 else 0 } +
            byteArrayOf(n.size.toByte()) + n + byteArrayOf(u.size.toByte()) + u
    }

    private fun lossyParseBody(bytes: ByteArray): ParsedBody = ParsedBody(
        kind = MessageBodyKind.CHAT,
        text = String(bytes, Charsets.UTF_8),
        attachments = emptyList(),
        target = null,
        emoji = null,
        isTyping = null,
    )

    private val decoded = FfiProfile(displayName = "Alice", username = "alice", updatedAtMs = 42uL)

    private fun classify(
        payload: ByteArray,
        decodeProfile: (ByteArray) -> FfiProfile?,
        resolveSender: (UInt) -> MemberIdentity? = { leaf -> "KEY_OF_LEAF_$leaf" },
        senderLeaf: UInt? = 3u,
    ): IncomingChatEvent = toEvent(
        messageId = 7L,
        createdAt = "2026-09-13T00:00:00Z",
        processed = ProcessedMessage.Application(payload, senderLeaf),
        resolveSender = resolveSender,
        isCommitterBeacon = { false },
        parseBody = ::lossyParseBody,
        decodeProfile = decodeProfile,
    )

    @Test
    fun `a profile envelope classifies as a profile bound to the resolved sender key`() {
        val event = classify(profileEnvelope(), decodeProfile = { decoded })
        assertTrue("expected Profile, got $event", event is IncomingChatEvent.Profile)
        event as IncomingChatEvent.Profile
        assertEquals(decoded, event.profile)
        assertEquals("KEY_OF_LEAF_3", event.senderKey)
    }

    @Test
    fun `a malformed profile envelope is dropped, never rendered as chat`() {
        val event = classify(profileEnvelope(name = "Alice‮"), decodeProfile = { null })
        assertTrue("expected Profile(null), got $event", event is IncomingChatEvent.Profile)
        assertNull((event as IncomingChatEvent.Profile).profile)
    }

    @Test
    fun `an unattributable sender yields no key to store under`() {
        val event = classify(profileEnvelope(), decodeProfile = { decoded }, senderLeaf = null)
        assertNull((event as IncomingChatEvent.Profile).senderKey)
    }

    @Test
    fun `a chat envelope is untouched by the profile arm`() {
        var decodeCalls = 0
        val chat = """{"v":1,"text":"hi","attachments":[]}""".toByteArray(Charsets.UTF_8)
        val event = classify(chat, decodeProfile = { decodeCalls++; decoded })
        assertTrue(event is IncomingChatEvent.Application)
        assertEquals(0, decodeCalls)
    }

    @Test
    fun `the prefix test needs the whole domain including its NUL`() {
        assertTrue(looksLikeProfileEnvelope(profileEnvelope()))
        assertTrue(looksLikeProfileEnvelope(profileDomain))
        assertFalse(looksLikeProfileEnvelope(profileDomain.copyOf(profileDomain.size - 1)))
        assertFalse(looksLikeProfileEnvelope("lakko/profile/v1 hello".toByteArray()))
        assertFalse(looksLikeProfileEnvelope(ByteArray(0)))
    }

    @Test
    fun `the background push filter suppresses a profile envelope`() {
        assertTrue(
            isServiceEnvelope(
                profileEnvelope(),
                decodesAsRoutingSecret = { false },
                decodesAsCommitterBeacon = { false },
                committerBeaconEnvelopeLen = 27,
            ),
        )
        // A malformed one too: it is still a profile envelope.
        assertTrue(
            isServiceEnvelope(
                profileEnvelope(name = "x".repeat(200)),
                decodesAsRoutingSecret = { false },
                decodesAsCommitterBeacon = { false },
                committerBeaconEnvelopeLen = 27,
            ),
        )
        assertFalse(
            isServiceEnvelope(
                """{"v":1,"text":"lakko/profile/v1","attachments":[]}""".toByteArray(),
                decodesAsRoutingSecret = { false },
                decodesAsCommitterBeacon = { false },
                committerBeaconEnvelopeLen = 27,
            ),
        )
    }
}
