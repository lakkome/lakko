// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Test

/** A join request's reply grant is cleared when it is refused: that one
 *  grant goes, nothing else held for the person. */
class WithoutHeldGrantTest {

    private val anna = "aa"
    private val bob = "bb"

    @Test
    fun `the current grant goes and the one it replaced takes its place`() {
        val held = listOf(
            StoredHeld(anna, "REPLY", "", "g2", "g1"),
            StoredHeld(anna, "CONTACT", "", "c1"),
            StoredHeld(bob, "REPLY", "", "g2"),
        )
        val out = withoutHeldGrant(held, listOf(anna), "REPLY", "g2")
        assertEquals(
            listOf(StoredHeld(anna, "REPLY", "", "g1", null), StoredHeld(anna, "CONTACT", "", "c1"), StoredHeld(bob, "REPLY", "", "g2")),
            out,
        )
    }

    @Test
    fun `a sole grant leaves with its entry, a replaced one alone, a stranger's nothing`() {
        val held = listOf(StoredHeld(anna, "REPLY", "", "g1"))
        assertEquals(emptyList<StoredHeld>(), withoutHeldGrant(held, listOf(anna), "REPLY", "g1"))
        val two = listOf(StoredHeld(anna, "REPLY", "", "g2", "g1"))
        assertEquals(listOf(StoredHeld(anna, "REPLY", "", "g2", null)), withoutHeldGrant(two, listOf(anna), "REPLY", "g1"))
        assertSame(held, withoutHeldGrant(held, listOf(anna), "REPLY", "other"))
        assertSame(held, withoutHeldGrant(held, listOf(bob), "REPLY", "g1"))
    }
}
