// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.settings

import me.lakko.app.reach.ReachApplyStatus
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** `docs/ux/who-can-find-me.md` sections 3, 4 and 8 — what the screen shows. */
class WhoCanFindMeModelTest {

    private val g1 = FindMeScope(FindMeScopeKind.GROUP, "AA11", "Hiking")
    private val g2 = FindMeScope(FindMeScopeKind.GROUP, "bb22", "alpha")
    private val c1 = FindMeScope(FindMeScopeKind.CHANNEL, "cc33", "News")

    @Test
    fun `the parts show only under Some people`() {
        val nobody = findMeRows(FindMeModel(FindMeMode.NOBODY, false, null, emptySet()), listOf(g1))
        assertFalse(nobody.showParts)
        assertFalse(nobody.showForm)
        val some = findMeRows(FindMeModel(FindMeMode.SOME, true, null, emptySet()), listOf(g1))
        assertTrue(some.showParts)
        assertFalse(some.showForm)
        val everyone = findMeRows(FindMeModel(FindMeMode.EVERYONE, false, FindMeForm.ALL, emptySet()), listOf(g1))
        assertFalse(everyone.showParts)
        assertFalse(everyone.showForm)
    }

    @Test
    fun `the form shows only while members is on`() {
        val rows = findMeRows(FindMeModel(FindMeMode.SOME, false, FindMeForm.ALL, emptySet()), listOf(g1))
        assertTrue(rows.showForm)
        assertEquals(FIND_ME_MEMBERS_ALL_HELPER, rows.membersHelper)
    }

    @Test
    fun `chosen counts N of M live, and a scope left since is not counted`() {
        val model = FindMeModel(
            FindMeMode.SOME,
            false,
            FindMeForm.CHOSEN,
            setOf(g1.chosenKey(), "GROUP:dead"),
        )
        val rows = findMeRows(model, listOf(g1, g2, c1))
        assertEquals(1, rows.chosenCount)
        assertEquals(3, rows.scopeCount)
        assertEquals("Only the ones you choose (1 of 3).", rows.membersHelper)
    }

    @Test
    fun `a chosen key is case-insensitive in the id and names its kind`() {
        assertEquals("GROUP:aa11", g1.chosenKey())
        assertEquals("CHANNEL:cc33", c1.chosenKey())
    }

    @Test
    fun `groups list before channels, each by name`() {
        assertEquals(listOf(g2, g1, c1), sortedFindMeScopes(listOf(c1, g1, g2)))
    }

    @Test
    fun `the status line says what has not happened yet`() {
        assertNull(findMeStatusLine(ReachApplyStatus.APPLIED))
        assertNull(findMeStatusLine(ReachApplyStatus.APPLYING))
        assertEquals(FIND_ME_OFFLINE, findMeStatusLine(ReachApplyStatus.OFFLINE))
        assertEquals(FIND_ME_FAILED, findMeStatusLine(ReachApplyStatus.FAILED))
    }
}
