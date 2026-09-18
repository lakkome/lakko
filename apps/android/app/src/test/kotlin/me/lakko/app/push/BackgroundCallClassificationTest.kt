// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The background path's classification, now that a call offer is CLASSIFIED
 * rather than suppressed.
 *
 * Suppressing it is why a closed app never rang (`docs/ux/calls.md` section 7):
 * the wake arrived, the offer decrypted, and the filter threw it away. The two
 * properties that have to hold together are that a call is still never rendered
 * as a message preview, and that it is no longer lost.
 */
class BackgroundCallClassificationTest {

    private val beaconLen = 27
    private val never: (ByteArray) -> Boolean = { false }

    private fun classify(payload: ByteArray, routingSecret: (ByteArray) -> Boolean = never) =
        classifyBackgroundPayload(payload, routingSecret, never, beaconLen)

    /** A `CallSignalingPayload` with one of the 1x1 oneof body tags. */
    private fun callFrame(tag: Int): ByteArray = byteArrayOf(tag.toByte(), 0x04, 0x0A, 0x02, 0x00, 0x00)

    @Test
    fun `every 1x1 call body is call signaling, not a message`() {
        // offer / answer / candidate / end.
        for (tag in listOf(0x52, 0x5A, 0x62, 0x6A)) {
            assertEquals(
                "tag 0x${tag.toString(16)}",
                BackgroundPayloadKind.CALL_SIGNALING,
                classify(callFrame(tag)),
            )
        }
    }

    @Test
    fun `a chat envelope is a message`() {
        assertEquals(
            BackgroundPayloadKind.MESSAGE,
            classify("""{"v":1,"text":"hi","attachments":[]}""".toByteArray(Charsets.UTF_8)),
        )
    }

    @Test
    fun `legacy plain text is a message, even when it opens with a call tag byte`() {
        // 0x72 and 0x7A are `r` and `z`: the group-control tags additionally
        // require the body's own `call_id` header, so typed text stays a message.
        for (text in listOf("rabbit", "zebra", "hello")) {
            assertEquals(text, BackgroundPayloadKind.MESSAGE, classify(text.toByteArray(Charsets.UTF_8)))
        }
    }

    @Test
    fun `a routing secret is a service envelope, not a call`() {
        val envelope = ByteArray(70).also { it[0] = 'l'.code.toByte() }
        assertEquals(BackgroundPayloadKind.SERVICE_ENVELOPE, classify(envelope) { true })
    }

    @Test
    fun `empty bytes are a message, which the chat parser then drops`() {
        assertEquals(BackgroundPayloadKind.MESSAGE, classify(ByteArray(0)))
    }

    @Test
    fun `call signaling is still never rendered as a message preview`() {
        // The question the chat-preview path asks is unchanged: "not a message"
        // still covers a call, so nothing can reach a notification body.
        for (tag in listOf(0x52, 0x6A)) {
            assertEquals(
                true,
                isServiceEnvelope(callFrame(tag), never, never, beaconLen),
            )
        }
    }
}
