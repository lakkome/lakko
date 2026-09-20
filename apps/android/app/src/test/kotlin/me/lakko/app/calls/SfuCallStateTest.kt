// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * `docs/ux/calls.md` section 4.3 — asking the SFU whether a call is still
 * there. The same URL and the same reading of the answer as web and iOS.
 */
class SfuCallStateTest {
    private val id = "ab".repeat(16)

    @Test
    fun `the state route is on the http origin behind the socket base`() {
        assertEquals(
            "https://test.lakko.me/sfu/v1/calls/$id?token=a.b.c",
            sfuCallStateUrl("wss://test.lakko.me/sfu", id, "a.b.c"),
        )
        assertEquals(
            "http://127.0.0.1:9640/v1/calls/$id?token=t",
            sfuCallStateUrl("ws://127.0.0.1:9640/", id, "t"),
        )
    }

    @Test
    fun `the token is escaped`() {
        assertTrue(sfuCallStateUrl("wss://h/sfu", id, "a b&c").contains("token=a+b%26c"))
    }

    @Test
    fun `the four words are read`() {
        assertEquals(SfuCallState.UNKNOWN, parseSfuCallState("""{"state":"unknown","peers":0}"""))
        assertEquals(SfuCallState.LIVE, parseSfuCallState("""{"state":"live","peers":2}"""))
        assertEquals(SfuCallState.CLOSING, parseSfuCallState("""{"state":"closing","peers":0}"""))
        assertEquals(SfuCallState.ENDED, parseSfuCallState("""{"state":"ended","peers":0}"""))
    }

    @Test
    fun `anything else is no answer, never ended`() {
        // A probe that cannot be read must never end a live call.
        assertNull(parseSfuCallState("""{"state":"gone"}"""))
        assertNull(parseSfuCallState("{}"))
        assertNull(parseSfuCallState("<html>"))
    }

    @Test
    fun `the probe is slower than the sfu grace`() {
        assertEquals(10_000L, SFU_CALL_STATE_PROBE_MS)
    }
}
