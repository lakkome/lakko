// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Host-side unit tests for the ADR-0037 §6 (G-11) pure rollback-detection
 * classifier [restoreVerdict]. Run as `:app:testDebugUnitTest` — no emulator,
 * no FFI, no Keystore (the classifier is a total pure function over
 * [RestoreGateFacts]). Mirror of the web `head-gate.test.ts` branch coverage.
 */
class HeadGateTest {

    @Test
    fun `absent head fails open`() {
        // Legacy blob / head stripped — restore proceeds (availability lane).
        val v = restoreVerdict(RestoreGateFacts(framed = false))
        assertTrue("expected FailOpen, got $v", v is RestoreVerdict.FailOpen)
    }

    @Test
    fun `truncated framed blob hard-fails`() {
        // A parse error (truncated / corrupt framed blob) is a hard tamper, and
        // it wins over every other fact (branch order is load-bearing).
        val v = restoreVerdict(
            RestoreGateFacts(parseError = true, framed = true, hasFloor = true),
        )
        assertTrue("expected HardFail, got $v", v is RestoreVerdict.HardFail)
    }

    @Test
    fun `returning invalid binding hard-fails`() {
        val v = restoreVerdict(
            RestoreGateFacts(framed = true, hasFloor = true, bindingInvalid = true),
        )
        assertTrue("expected HardFail, got $v", v is RestoreVerdict.HardFail)
    }

    @Test
    fun `fresh invalid head hard-fails`() {
        val v = restoreVerdict(
            RestoreGateFacts(framed = true, hasFloor = false, freshInvalid = true),
        )
        assertTrue("expected HardFail, got $v", v is RestoreVerdict.HardFail)
    }

    @Test
    fun `returning hash mismatch hard-fails`() {
        val v = restoreVerdict(
            RestoreGateFacts(framed = true, hasFloor = true, hashMismatch = true),
        )
        assertTrue("expected HardFail, got $v", v is RestoreVerdict.HardFail)
    }

    @Test
    fun `returning version below floor is stale override`() {
        val v = restoreVerdict(
            RestoreGateFacts(framed = true, hasFloor = true, versionBelowFloor = true),
        )
        assertTrue("expected StaleOverride, got $v", v is RestoreVerdict.StaleOverride)
    }

    @Test
    fun `fresh stale head is stale override`() {
        val v = restoreVerdict(
            RestoreGateFacts(framed = true, hasFloor = false, freshStale = true),
        )
        assertTrue("expected StaleOverride, got $v", v is RestoreVerdict.StaleOverride)
    }

    @Test
    fun `returning clean head proceeds`() {
        val v = restoreVerdict(RestoreGateFacts(framed = true, hasFloor = true))
        assertEquals(RestoreVerdict.Proceed, v)
    }

    @Test
    fun `fresh clean head proceeds`() {
        val v = restoreVerdict(RestoreGateFacts(framed = true, hasFloor = false))
        assertEquals(RestoreVerdict.Proceed, v)
    }

    @Test
    fun `binding invalid wins over hash and version in the returning path`() {
        // Branch order: a bad signature/binding is judged before hash / floor.
        val v = restoreVerdict(
            RestoreGateFacts(
                framed = true,
                hasFloor = true,
                bindingInvalid = true,
                hashMismatch = true,
                versionBelowFloor = true,
            ),
        )
        assertTrue("expected HardFail, got $v", v is RestoreVerdict.HardFail)
    }

    @Test
    fun `hash mismatch wins over version below floor`() {
        val v = restoreVerdict(
            RestoreGateFacts(
                framed = true,
                hasFloor = true,
                hashMismatch = true,
                versionBelowFloor = true,
            ),
        )
        assertTrue("expected HardFail, got $v", v is RestoreVerdict.HardFail)
    }

    @Test
    fun `staleness millis is seconds times 1000`() {
        // The default (no BuildConfig override in unit tests) is 30 days.
        assertEquals(BACKUP_HEAD_STALENESS_SECS_DEFAULT * 1000L, backupHeadStalenessMillis())
    }
}
