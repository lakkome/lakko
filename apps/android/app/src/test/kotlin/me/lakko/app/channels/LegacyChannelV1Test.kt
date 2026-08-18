// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Retired-v1 PublisherGroup error mapping (Kotlin mirror of the web
 * `legacy-v1.test.ts` and the iOS `LegacyChannelV1Tests`).
 *
 * The point of these tests is **discrimination**, not just that the new copy
 * appears: a network / server / codec failure must keep its own text, or the
 * mapping has simply swallowed every channel error.
 */
class LegacyChannelV1Test {

    private companion object {
        /**
         * The refusal `refuse_if_legacy_v1_present` raises, as it reaches Kotlin
         * through UniFFI's `FfiException.Group` carrying `Error::Group`'s payload.
         */
        const val RUST_REFUSAL =
            "legacy v1 PublisherGroup state present for this channel: the v1 group id was " +
                "41 bytes, which the Delivery Service rejects, and an MLS group id is bound " +
                "into the GroupContext so it cannot be re-keyed to the 32-byte v2 id. The v1 " +
                "state is left intact; this channel needs a migration or a fresh channel."
    }

    private fun v1GroupId(fill: Byte = 0xAB.toByte()): ByteArray =
        LegacyChannelV1.V1_PREFIX + ByteArray(16) { fill }

    private fun v2GroupId(fill: Byte = 0xAB.toByte()): ByteArray =
        "lakko/pubgrp/v2/".toByteArray(Charsets.US_ASCII) + ByteArray(16) { fill }

    // ---------------------------------------------------------------
    // Structural handle: the dead invite link
    // ---------------------------------------------------------------

    @Test
    fun `recognises a 41-byte v1 group id`() {
        assertTrue(LegacyChannelV1.isLegacyV1PublisherGroupId(v1GroupId()))
    }

    @Test
    fun `rejects a current v2 publisher group id`() {
        assertEquals(32, v2GroupId().size)
        assertFalse(LegacyChannelV1.isLegacyV1PublisherGroupId(v2GroupId()))
    }

    @Test
    fun `rejects a 32-byte chat group id`() {
        assertFalse(LegacyChannelV1.isLegacyV1PublisherGroupId(ByteArray(32) { 0x7F }))
    }

    @Test
    fun `rejects 41 bytes without the v1 prefix`() {
        assertFalse(LegacyChannelV1.isLegacyV1PublisherGroupId(ByteArray(41)))
    }

    @Test
    fun `rejects an empty id`() {
        assertFalse(LegacyChannelV1.isLegacyV1PublisherGroupId(ByteArray(0)))
    }

    // ---------------------------------------------------------------
    // String handle: the unopenable channel
    // ---------------------------------------------------------------

    @Test
    fun `recognises the rust refusal`() {
        assertTrue(LegacyChannelV1.isLegacyPublisherGroupError(Exception(RUST_REFUSAL)))
        assertTrue(LegacyChannelV1.isLegacyRefusalText(RUST_REFUSAL))
    }

    /** Discrimination: the unrelated failures must NOT be captured. */
    @Test
    fun `does not fire on unrelated failures`() {
        val unrelated = listOf(
            "a network failure" to java.io.IOException("Unable to resolve host lakko.me"),
            "a server rejection" to Exception("channels service returned 503"),
            "a bad channel id" to CoPublisher.CoPublisherException.BadChannelId("zzzz"),
            "an unrelated MLS group error" to Exception("group operation failed: no such group"),
            "a chat-group /g/ link" to CoPublisher.CoPublisherException.NotAChannelGroup,
            "a null message" to Exception(),
        )
        for ((label, err) in unrelated) {
            assertFalse("should not fire on $label", LegacyChannelV1.isLegacyPublisherGroupError(err))
        }
    }

    @Test
    fun `refusal text predicate rejects unrelated text`() {
        assertFalse(LegacyChannelV1.isLegacyRefusalText("group operation failed: no such group"))
        assertFalse(LegacyChannelV1.isLegacyRefusalText("Unable to resolve host lakko.me"))
        assertFalse(LegacyChannelV1.isLegacyRefusalText(""))
        assertFalse(LegacyChannelV1.isLegacyRefusalText(null))
    }

    // ---------------------------------------------------------------
    // Mapping
    // ---------------------------------------------------------------

    @Test
    fun `mapOpenError converts the refusal`() {
        val mapped = LegacyChannelV1.mapOpenError(Exception(RUST_REFUSAL))
        assertTrue("expected a LegacyChannelException, got $mapped", mapped is LegacyChannelException)
        assertEquals(LegacyChannelKind.CHANNEL, (mapped as LegacyChannelException).kind)
        assertEquals(LegacyChannelV1.CHANNEL_MESSAGE, mapped.message)
    }

    @Test
    fun `mapOpenError leaves an unrelated failure untouched`() {
        val original = java.io.IOException("Unable to resolve host lakko.me")
        val mapped = LegacyChannelV1.mapOpenError(original)
        assertFalse(mapped is LegacyChannelException)
        assertTrue(mapped === original)
    }

    @Test
    fun `describe returns the channel copy for the refusal`() {
        val msg = LegacyChannelV1.describe(Exception(RUST_REFUSAL)) { "fallback: ${it.message}" }
        assertEquals(LegacyChannelV1.CHANNEL_MESSAGE, msg)
    }

    @Test
    fun `describe returns the invite link copy for a link error`() {
        val msg = LegacyChannelV1.describe(LegacyChannelException(LegacyChannelKind.INVITE_LINK)) {
            "fallback: ${it.message}"
        }
        assertEquals(LegacyChannelV1.CO_PUBLISHER_LINK_MESSAGE, msg)
    }

    /** Discrimination: a network failure still reads as a network failure. */
    @Test
    fun `describe falls back for a network failure`() {
        val original = java.io.IOException("Unable to resolve host lakko.me")
        val msg = LegacyChannelV1.describe(original) {
            "Could not open the channel group: ${it.message}"
        }
        assertTrue(msg.contains("Unable to resolve host"))
        assertNotEquals(LegacyChannelV1.CHANNEL_MESSAGE, msg)
    }

    // ---------------------------------------------------------------
    // Copy
    // ---------------------------------------------------------------

    @Test
    fun `channel copy says all three things in order`() {
        val msg = LegacyChannelV1.CHANNEL_MESSAGE
        val older = msg.indexOf("older version")
        val readable = msg.indexOf("stay readable")
        val create = msg.indexOf("create a new channel")
        assertTrue(older >= 0)
        assertTrue(readable > older)
        assertTrue(create > readable)
        assertTrue(msg.contains("cannot be repaired"))
    }

    @Test
    fun `copy offers no repair or retry affordance`() {
        for (msg in listOf(LegacyChannelV1.CHANNEL_MESSAGE, LegacyChannelV1.CO_PUBLISHER_LINK_MESSAGE)) {
            val lowered = msg.lowercase()
            for (forbidden in listOf("migrat", "retry", "try again", "repair this", "recover")) {
                assertFalse("copy must not offer $forbidden", lowered.contains(forbidden))
            }
        }
    }

    @Test
    fun `invite link copy tells the joiner to ask for a new link`() {
        assertTrue(LegacyChannelV1.CO_PUBLISHER_LINK_MESSAGE.contains("older version"))
        assertTrue(LegacyChannelV1.CO_PUBLISHER_LINK_MESSAGE.contains("new invite link"))
    }

    /** The copy must be byte-identical to the web + iOS ports. */
    @Test
    fun `copy matches the cross-platform wording`() {
        assertEquals(
            "This channel was created by an older version of Lakko and cannot be opened in this " +
                "build. Posts you already published stay readable for your subscribers. " +
                "To publish again, create a new channel — this one cannot be repaired.",
            LegacyChannelV1.CHANNEL_MESSAGE,
        )
        assertEquals(
            "This co-publisher invite link was created by an older version of Lakko and no " +
                "longer works. Ask the channel owner to send you a new invite link.",
            LegacyChannelV1.CO_PUBLISHER_LINK_MESSAGE,
        )
    }
}
