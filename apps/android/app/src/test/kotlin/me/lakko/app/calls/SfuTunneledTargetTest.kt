// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** ADR-0026 — the tunneled SFU signaling socket reaches the same path as the direct one. */
class SfuTunneledTargetTest {
    @Test
    fun keepsThePathPrefix() {
        assertEquals(
            TunneledSignalingTarget("test.lakko.me", 443.toUShort(), "ws://test.lakko.me/sfu/v1/connect?token=a.b.c"),
            tunneledSignalingTarget("wss://test.lakko.me/sfu", "a.b.c"),
        )
    }

    @Test
    fun trailingSlashesAreTrimmed() {
        assertEquals(
            "ws://test.lakko.me/sfu/v1/connect?token=t",
            tunneledSignalingTarget("wss://test.lakko.me/sfu//", "t")?.innerUrl,
        )
    }

    @Test
    fun originOnlyBaseHasNoPrefix() {
        assertEquals(
            "ws://sfu.lakko.me/v1/connect?token=t",
            tunneledSignalingTarget("wss://sfu.lakko.me", "t")?.innerUrl,
        )
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
