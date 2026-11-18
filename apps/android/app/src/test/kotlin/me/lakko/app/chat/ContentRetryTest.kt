// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.MessageContent

/** ADR-0090 — which failed content rows a Retry may send as they are. */
class ContentRetryTest {
    private val hash = ByteArray(32) { 9 }

    private fun poll(anonymous: uniffi.lakko_ffi_uniffi.PollAnonymous?) = MessageContent.Poll(
        uniffi.lakko_ffi_uniffi.SignedPoll(
            uniffi.lakko_ffi_uniffi.PollBody(
                id = ByteArray(16),
                question = "q",
                options = listOf("a", "b"),
                multiple = false,
                quiz = null,
                closeAtMs = null,
                anonymous = anonymous,
            ),
            null,
        ),
    )

    @Test
    fun `every row but a changed-roster anonymous poll is retried as it is`() {
        val never: (ByteArray) -> Boolean = { error("not asked") }
        assertTrue(retryableAsIs(null, never))
        assertTrue(retryableAsIs(poll(null), never))
        assertTrue(
            retryableAsIs(
                MessageContent.Location(
                    uniffi.lakko_ffi_uniffi.SharedLocation(latE7 = 1, lonE7 = 2, accuracyM = 3u, fixAtMs = 4L, name = null),
                ),
                never,
            ),
        )
        assertTrue(
            retryableAsIs(
                MessageContent.ContactCard(uniffi.lakko_ffi_uniffi.SharedContactCard("Bob", "bob", ByteArray(32))),
                never,
            ),
        )
    }

    @Test
    fun `an anonymous poll is retried only while the roster still gives its ring hash`() {
        val anon = poll(uniffi.lakko_ffi_uniffi.PollAnonymous(secret = ByteArray(32), ring0Hash = hash))
        var asked: ByteArray? = null
        assertTrue(retryableAsIs(anon) { asked = it; true })
        assertTrue(asked!!.contentEquals(hash))
        assertFalse(retryableAsIs(anon) { false })
    }

    @Test
    fun `a refused anonymous poll retry says poll resend refused`() {
        val text = chatErrorText(anonymousPollResendRefused(), "chat.send.failed", { false })
        org.junit.Assert.assertEquals("poll.resend_refused", text.key)
    }
}
