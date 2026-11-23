// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * A history poll that ends without a bundle says so in one plain line (seen
 * live 2026-11-23: a device added from web got no history and the log showed
 * nothing about it).
 */
class HistoryOutcomeLogTest {

    @Test
    fun `a timeout is logged as a plain line`() {
        assertEquals(
            "history catch-up: no bundle arrived before the timeout",
            WelcomeCourier.historyOutcomeLogLine(WelcomeCourier.HistoryBundleOutcome.Timeout),
        )
    }

    @Test
    fun `a cancelled poll is logged`() {
        assertEquals(
            "history catch-up: cancelled",
            WelcomeCourier.historyOutcomeLogLine(WelcomeCourier.HistoryBundleOutcome.Cancelled),
        )
    }

    @Test
    fun `a received bundle logs nothing here and the line names nothing`() {
        assertNull(
            WelcomeCourier.historyOutcomeLogLine(WelcomeCourier.HistoryBundleOutcome.Received(byteArrayOf(1, 2, 3))),
        )
        val line = WelcomeCourier.historyOutcomeLogLine(WelcomeCourier.HistoryBundleOutcome.Timeout).orEmpty()
        assertEquals(false, line.any { it.isDigit() })
    }
}
