// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0060 section 4, "Narrowing": the `fof` key rotates every seven days as
 * well as on removing a contact, as on web (`reach-model.ts`
 * `fofRotationDue` / `rotateFof`). Host JVM only.
 */
class FofScheduleTest {

    private val day = 24L * 60 * 60 * 1000

    @Test
    fun `the schedule is seven days`() {
        assertEquals(7 * day, FOF_ROTATION_MS)
    }

    @Test
    fun `a rotation is due seven days after the last one and not before`() {
        val last = 1_000_000L
        assertFalse(fofRotationDue(last, last))
        assertFalse(fofRotationDue(last, last + 7 * day - 1))
        assertTrue(fofRotationDue(last, last + 7 * day))
        assertTrue(fofRotationDue(last, last + 30 * day))
    }

    @Test
    fun `the first pass on a device only starts the schedule`() {
        // Install, restore, pairing: no "rotated at" is held.
        val now = 1_700_000_000_000L
        val fresh = ReachDoc()
        assertNull(fresh.fofRotatedAtMs)
        assertFalse(fofRotationDue(fresh.fofRotatedAtMs, now))
        assertEquals(now, fofScheduleStart(fresh.fofRotatedAtMs, now))
        // Once started, the next due point is seven days on, not now.
        assertFalse(fofRotationDue(now, now + 7 * day - 1))
        assertTrue(fofRotationDue(now, now + 7 * day))
        // A held "rotated at" is never restarted by a later pass.
        assertNull(fofScheduleStart(now, now + 8 * day))
    }

    @Test
    fun `a clock that went backwards reads as not due`() {
        val last = 1_700_000_000_000L
        assertFalse(fofRotationDue(last, last - 1))
        assertFalse(fofRotationDue(last, last - 30 * day))
    }

    @Test
    fun `the scheduled rotation raises the fof version and restarts the schedule`() {
        val now = 1_700_000_000_000L
        val doc = ReachDoc(
            versions = listOf(
                StoredVersion(ReachKind.FOF.name, "", 3),
                StoredVersion(ReachKind.CONTACT.name, "", 5),
            ),
            fofRotatedAtMs = now - 8 * day,
        )
        val next = withScheduledFofRotation(doc, now)
        assertEquals(4, next.versions.single { it.kind == ReachKind.FOF.name }.version)
        assertEquals(5, next.versions.single { it.kind == ReachKind.CONTACT.name }.version)
        assertEquals(now, next.fofRotatedAtMs)
        assertFalse(fofRotationDue(next.fofRotatedAtMs, now + 7 * day - 1))
    }

    @Test
    fun `the first scheduled rotation takes fof from 0 to 1`() {
        val next = withScheduledFofRotation(ReachDoc(), 42L)
        assertEquals(1, next.versions.single { it.kind == ReachKind.FOF.name }.version)
    }

    @Test
    fun `the old fof key comes off the mailbox in the same reconcile`() {
        val before = ReachDoc(versions = listOf(StoredVersion(ReachKind.FOF.name, "", 2)))
        val after = withScheduledFofRotation(before, 7L)
        fun desired(doc: ReachDoc): Map<String, DesiredReachKey> = desiredReachKeys(
            ReachInputs(
                versions = doc.versions.associate { ReachSlot(ReachKind.valueOf(it.kind), it.scopeHex) to it.version },
                joinedGroups = emptySet(),
                visibleGroups = emptySet(),
                visibleChannels = emptySet(),
                contactsOfContacts = true,
                links = emptyMap(),
                replies = emptyMap(),
            ),
            nowMs = 7L,
        ).associateBy { "${it.slot.kind}:${it.version}" } // stands in for the derived public key
        val registered = desired(before).mapValues { (_, k) -> RegisteredReachKey(k.slot, k.version, k.expiresAtMs) }
        val diff = diffReachKeys(desired(after), registered)
        assertEquals(setOf("FOF:2"), diff.remove)
        assertEquals(setOf("FOF:3"), diff.add.keys)
    }

    @Test
    fun `the schedule survives a restart in the stored document`() {
        val json = Json {
            ignoreUnknownKeys = true
            explicitNulls = false
        }
        val stored = json.encodeToString(ReachDoc.serializer(), ReachDoc(fofRotatedAtMs = 123_456L))
        assertEquals(123_456L, json.decodeFromString(ReachDoc.serializer(), stored).fofRotatedAtMs)
        // A document written before the field existed holds no schedule yet:
        // its next pass starts one and rotates nothing.
        assertNull(json.decodeFromString(ReachDoc.serializer(), "{}").fofRotatedAtMs)
    }

    @Test
    fun `another device's fof rotation restarts this device's schedule`() {
        val now = 1_700_000_000_000L
        val last = now - 6 * day
        assertEquals(now, fofRotatedAtAfterAdopt(fofBefore = 2, fofAfter = 3, fofRotatedAtMs = last, nowMs = now))
        assertFalse(fofRotationDue(now, now + 6 * day))
    }

    @Test
    fun `an adopted state that did not raise fof leaves the schedule alone`() {
        val now = 1_700_000_000_000L
        val last = now - 6 * day
        assertEquals(last, fofRotatedAtAfterAdopt(fofBefore = 3, fofAfter = 3, fofRotatedAtMs = last, nowMs = now))
        assertNull(fofRotatedAtAfterAdopt(fofBefore = 3, fofAfter = 3, fofRotatedAtMs = null, nowMs = now))
    }

    @Test
    fun `another device's higher fof starts a schedule this device did not hold yet`() {
        val now = 1_700_000_000_000L
        assertEquals(now, fofRotatedAtAfterAdopt(fofBefore = 0, fofAfter = 1, fofRotatedAtMs = null, nowMs = now))
    }

    @Test
    fun `a clock that went back never moves the schedule backwards`() {
        val last = 1_700_000_000_000L
        assertEquals(last, fofRotatedAtAfterAdopt(fofBefore = 1, fofAfter = 2, fofRotatedAtMs = last, nowMs = last - day))
    }
}
