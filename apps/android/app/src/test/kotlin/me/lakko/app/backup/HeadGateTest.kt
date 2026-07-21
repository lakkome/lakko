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

    // ---------------------------------------------------------------------
    // F4 — absent head split on the floor (returning client, head stripped).
    // ---------------------------------------------------------------------

    @Test
    fun `absent head with no floor fails open`() {
        // A true zero-anchor fresh install with a headless blob — the only
        // remaining fail-open case.
        val v = restoreVerdict(RestoreGateFacts(framed = false, hasFloor = false))
        assertTrue("expected FailOpen, got $v", v is RestoreVerdict.FailOpen)
    }

    @Test
    fun `absent head byte-identical to floor proceeds`() {
        // F4: headless but BLAKE3(envelope) == floor.envelopeHash — stripped
        // benignly in transit, byte-identical to the last known head → proceed.
        val v = restoreVerdict(
            RestoreGateFacts(
                framed = false,
                hasFloor = true,
                absentHashMatchesFloor = true,
            ),
        )
        assertEquals(RestoreVerdict.Proceed, v)
    }

    @Test
    fun `absent head differing from floor is stale override`() {
        // F4: a returning client with head-protected history gets a DIFFERENT
        // headless envelope — a head-stripping downgrade → stale-override, never
        // fail-open.
        val v = restoreVerdict(
            RestoreGateFacts(
                framed = false,
                hasFloor = true,
                absentHashMatchesFloor = false,
            ),
        )
        assertTrue("expected StaleOverride, got $v", v is RestoreVerdict.StaleOverride)
    }

    // ---------------------------------------------------------------------
    // F5a — corrupt floor dominates (untrustworthy local anchor).
    // ---------------------------------------------------------------------

    @Test
    fun `corrupt floor with absent head is stale override`() {
        // The persisted anchor exists but is undecryptable — never demote to the
        // weaker fresh path; require an explicit acknowledgement.
        val v = restoreVerdict(RestoreGateFacts(framed = false, floorCorrupt = true))
        assertTrue("expected StaleOverride, got $v", v is RestoreVerdict.StaleOverride)
    }

    @Test
    fun `corrupt floor with framed head is stale override`() {
        // A framed head is still not trusted over a corrupt anchor — the corrupt
        // floor routes to stale-override before the framed verify path runs.
        val v = restoreVerdict(RestoreGateFacts(framed = true, floorCorrupt = true))
        assertTrue("expected StaleOverride, got $v", v is RestoreVerdict.StaleOverride)
    }

    @Test
    fun `parse error wins over corrupt floor`() {
        // Branch order: a truncated framed blob is a hard tamper even with a
        // corrupt floor present.
        val v = restoreVerdict(
            RestoreGateFacts(parseError = true, floorCorrupt = true),
        )
        assertTrue("expected HardFail, got $v", v is RestoreVerdict.HardFail)
    }

    @Test
    fun `corrupt floor wins over the framed returning path`() {
        // Branch order: floorCorrupt dominates a framed + hasFloor state that
        // would otherwise HardFail on the bad binding — the anchor is judged
        // untrustworthy first.
        val v = restoreVerdict(
            RestoreGateFacts(
                framed = true,
                floorCorrupt = true,
                hasFloor = true,
                bindingInvalid = true,
                hashMismatch = true,
                versionBelowFloor = true,
            ),
        )
        assertTrue("expected StaleOverride, got $v", v is RestoreVerdict.StaleOverride)
    }

    @Test
    fun `staleness millis is seconds times 1000`() {
        // The default (no BuildConfig override in unit tests) is 30 days.
        assertEquals(BACKUP_HEAD_STALENESS_SECS_DEFAULT * 1000L, backupHeadStalenessMillis())
    }
}
