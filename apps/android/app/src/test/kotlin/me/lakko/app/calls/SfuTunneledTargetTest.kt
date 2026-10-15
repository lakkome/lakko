// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * ADR-0026 — the tunneled SFU signaling socket reaches the same path as the
 * direct one, with the admission token in the offered subprotocols.
 */
class SfuTunneledTargetTest {
    @Test
    fun keepsThePathPrefix() {
        assertEquals(
            TunneledSignalingTarget(
                "test.lakko.me",
                443.toUShort(),
                "ws://test.lakko.me/sfu/v1/connect",
                "lakko-sfu-v1,lakko-sfu-token.a.b.c",
            ),
            tunneledSignalingTarget("wss://test.lakko.me/sfu", "a.b.c"),
        )
    }

    @Test
    fun trailingSlashesAreTrimmed() {
        assertEquals(
            "ws://test.lakko.me/sfu/v1/connect",
            tunneledSignalingTarget("wss://test.lakko.me/sfu//", "t")?.innerUrl,
        )
    }

    @Test
    fun originOnlyBaseHasNoPrefix() {
        assertEquals(
            "ws://sfu.lakko.me/v1/connect",
            tunneledSignalingTarget("wss://sfu.lakko.me", "t")?.innerUrl,
        )
    }

    /** The admission token rides `Sec-WebSocket-Protocol` beside
     *  `lakko-sfu-v1`, never the URL (invariant #5); the direct path sends the
     *  same header value. */
    @Test
    fun theTokenIsASubprotocolNotAQuery() {
        val target = tunneledSignalingTarget("wss://sfu.lakko.me", "x.y.z")
        assertFalse(target!!.innerUrl.contains("x.y.z"))
        assertEquals("lakko-sfu-v1,lakko-sfu-token.x.y.z", target.subprotocols)
        assertEquals("lakko-sfu-v1,lakko-sfu-token.x.y.z", sfuSubprotocolHeader("x.y.z"))
    }

    @Test
    fun explicitPortAndPlainScheme() {
        val target = tunneledSignalingTarget("ws://10.0.0.2:7880", "t")
        assertEquals("10.0.0.2", target?.host)
        assertEquals(7880.toUShort(), target?.port)
    }

    @Test
    fun noHostIsRefused() {
        assertNull(tunneledSignalingTarget("not a url", "t"))
    }
}
