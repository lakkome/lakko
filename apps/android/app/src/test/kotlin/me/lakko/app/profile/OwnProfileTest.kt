// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0048 / UX §4 — the own profile: every change is stamped strictly newer
 * (receivers keep only a strictly newer profile), an unchanged save is not a
 * change, and the live preview matches the spec's shapes.
 */
class OwnProfileTest {

    @Test
    fun `a change is stamped strictly newer even when the clock went back`() {
        assertEquals(1_000L, nextProfileStamp(previousMs = null, nowMs = 1_000L))
        assertEquals(2_000L, nextProfileStamp(previousMs = 1_000L, nowMs = 2_000L))
        // Clock moved backwards, or two saves in the same millisecond.
        assertEquals(1_001L, nextProfileStamp(previousMs = 1_000L, nowMs = 500L))
        assertEquals(1_001L, nextProfileStamp(previousMs = 1_000L, nowMs = 1_000L))
    }

    @Test
    fun `saving the same name is not a change`() {
        val current = OwnProfile("Alice", "alice", 10L)
        assertNull(applyOwnNameChange(current, "Alice", "alice", nowMs = 99L))
    }

    @Test
    fun `a new name gets a strictly newer stamp`() {
        val current = OwnProfile("Alice", "alice", 10L)
        val updated = applyOwnNameChange(current, "Alice Petrova", "alice", nowMs = 5L)
        assertEquals("Alice Petrova", updated?.displayName)
        assertTrue((updated?.updatedAtMs ?: 0L) > current.updatedAtMs)
        // Clearing the name is a change too.
        assertEquals(null, applyOwnNameChange(current, null, "alice", 20L)?.displayName)
        assertEquals(20L, applyOwnNameChange(current, null, "alice", 20L)?.updatedAtMs)
    }

    @Test
    fun `the preview shows how others see this account`() {
        assertEquals("Alice Petrova · @alice", ownProfilePreview(DisplayNameCheck.Valid("Alice Petrova"), "alice"))
        assertEquals("@alice", ownProfilePreview(DisplayNameCheck.Valid(null), "alice"))
        assertEquals("Guest", ownProfilePreview(DisplayNameCheck.Valid("Guest"), null))
        // A refused name will not be sent, so it previews as no name.
        assertEquals(
            "@alice",
            ownProfilePreview(DisplayNameCheck.Invalid(DisplayNameProblem.LINE_BREAK), "alice"),
        )
    }
}
