// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import android.content.Context
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import uniffi.lakko_ffi_uniffi.FfiPushProvider

/**
 * ADR-0078 section 3 — the device-side state of the sealed subscriptions:
 * the FCM token kept on the device (a change is what re-sends every
 * subscription), and the subscription ids a wipe hands to the sign-out's
 * unsubscribe.
 */
@RunWith(RobolectricTestRunner::class)
class PushDeviceTokensTest {

    private lateinit var context: Context

    @Before
    fun setUp() {
        context = RuntimeEnvironment.getApplication()
    }

    /** Plain preferences stand in for the Keystore-backed file, which the
     *  host JVM cannot open. */
    private fun tokens(): PushDeviceTokens {
        val prefs = context.getSharedPreferences("push-device-tokens-test", Context.MODE_PRIVATE)
        prefs.edit().clear().commit()
        return PushDeviceTokens { prefs }
    }

    @Test
    fun `only a changed token is a change`() {
        val tokens = tokens()
        assertTrue(tokens.setFcm("fcm-1"))
        assertFalse("the same token again is not a change", tokens.setFcm("fcm-1"))
        assertTrue(tokens.setFcm("fcm-2"))
        assertEquals("fcm-2", tokens.fcm)
    }

    @Test
    fun `an empty or over-long token is never stored`() {
        val tokens = tokens()
        tokens.setFcm("")
        assertNull(tokens.fcm)
        tokens.setFcm("x".repeat(PushDeviceTokens.MAX_TOKEN_BYTES + 1))
        assertNull(tokens.fcm)
        assertTrue(tokens.entries().isEmpty(), "no token, nothing to subscribe")
    }

    @Test
    fun `the token rides a subscription under the FCM provider`() {
        val tokens = tokens()
        tokens.setFcm("fcm-1")
        val entries = tokens.entries()
        assertEquals(1, entries.size)
        assertEquals(FfiPushProvider.FCM, entries.single().provider)
        assertTrue(entries.single().token.contentEquals("fcm-1".toByteArray()))
        tokens.clear()
        assertNull(tokens.fcm)
    }

    @Test
    fun `stashed subscription ids drain once`() {
        val pending = PendingPushUnsubscribes(context)
        pending.drain()
        val a = ByteArray(16) { 1 }
        val b = ByteArray(16) { 2 }
        pending.stash(listOf(a, b))
        pending.stash(listOf(a))
        val drained = pending.drain().map { PushClient.pseudonymHex(it) }.toSet()
        assertEquals(setOf(PushClient.pseudonymHex(a), PushClient.pseudonymHex(b)), drained)
        assertTrue(pending.drain().isEmpty())
    }

    @Test
    fun `a pinned wake key is used and a malformed pin is ignored`() {
        val key = ByteArray(32) { 7 }
        val pin = java.util.Base64.getUrlEncoder().withoutPadding().encodeToString(key)
        assertTrue(key.contentEquals(PushWakeKey.pinned(pin)))
        assertNull(PushWakeKey.pinned(""))
        assertNull(PushWakeKey.pinned("not base64 !"))
        assertNull(PushWakeKey.pinned(java.util.Base64.getUrlEncoder().encodeToString(ByteArray(16))))
    }

    private fun assertTrue(condition: Boolean, message: String) = assertTrue(message, condition)
}
