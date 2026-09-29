// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import me.lakko.app.push.BackgroundPayloadKind
import me.lakko.app.push.classifyBackgroundPayload
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * ADR-0058 — the scoped-discovery envelopes are dispatched and never rendered,
 * on the foreground classifier and in the background push filter alike.
 */
class ScopedEnvelopesTest {

    private fun envelope(domain: String, vararg tail: Int): ByteArray =
        domain.toByteArray(Charsets.US_ASCII) + byteArrayOf(0) + tail.map { it.toByte() }.toByteArray()

    @Test
    fun `each frozen domain classifies to its kind`() {
        val expected = mapOf(
            "lakko/introduction-offer/v1" to ScopedEnvelopeKind.INTRODUCTION_OFFER,
            "lakko/introductions/v1" to ScopedEnvelopeKind.INTRODUCTIONS,
            "lakko/visibility/v1" to ScopedEnvelopeKind.VISIBILITY,
            "lakko/reach-state/v1" to ScopedEnvelopeKind.REACH_STATE,
            "lakko/committer-delegation/v1" to ScopedEnvelopeKind.COMMITTER_DELEGATION,
        )
        for ((domain, kind) in expected) {
            assertEquals(domain, kind, scopedEnvelopeKindOf(envelope(domain, 1, 2, 3)))
        }
    }

    @Test
    fun `a domain that does not decode is still that kind, so it is dropped rather than rendered`() {
        assertEquals(ScopedEnvelopeKind.VISIBILITY, scopedEnvelopeKindOf(envelope("lakko/visibility/v1")))
    }

    @Test
    fun `chat text and near misses are not service envelopes`() {
        assertNull(scopedEnvelopeKindOf("""{"v":1,"text":"hi"}""".toByteArray()))
        assertNull(scopedEnvelopeKindOf("lakko/visibility/v1 is a word".toByteArray()))
        assertNull(scopedEnvelopeKindOf("lakko/visibility/v".toByteArray()))
        assertNull(scopedEnvelopeKindOf(ByteArray(0)))
        // Another service domain (the profile) is not one of these.
        assertNull(scopedEnvelopeKindOf(envelope("lakko/profile/v1", 1)))
    }

    @Test
    fun `the background push filter suppresses every scoped kind`() {
        for (kind in ScopedEnvelopeKind.entries) {
            val bytes = kind.prefix + byteArrayOf(9, 9)
            val classified = classifyBackgroundPayload(
                bytes,
                decodesAsRoutingSecret = { false },
                decodesAsCommitterBeacon = { false },
                committerBeaconEnvelopeLen = 27,
            )
            assertEquals(kind.name, BackgroundPayloadKind.SERVICE_ENVELOPE, classified)
        }
    }

    @Test
    fun `the background push filter still lets a chat message through`() {
        val classified = classifyBackgroundPayload(
            """{"v":1,"text":"lakko/visibility/v1"}""".toByteArray(),
            decodesAsRoutingSecret = { false },
            decodesAsCommitterBeacon = { false },
            committerBeaconEnvelopeLen = 27,
        )
        assertEquals(BackgroundPayloadKind.MESSAGE, classified)
    }
}
