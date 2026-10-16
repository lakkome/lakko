// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * `docs/ux/calls.md` section 4.3 — asking the SFU whether a call is still
 * there. The same URL and the same reading of the answer as web and iOS.
 */
class SfuCallStateTest {
    private val room = "ab".repeat(32)

    /** ADR-0078 section 2: the route names the room and carries no token. */
    @Test
    fun `the state route is on the http origin behind the socket base`() {
        assertEquals(
            "https://test.lakko.me/sfu/v1/rooms/$room",
            sfuCallStateUrl("wss://test.lakko.me/sfu", room),
        )
        assertEquals(
            "http://127.0.0.1:9640/v1/rooms/$room",
            sfuCallStateUrl("ws://127.0.0.1:9640/", room),
        )
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
