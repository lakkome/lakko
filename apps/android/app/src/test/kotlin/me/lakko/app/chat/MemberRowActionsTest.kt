// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Test

/** The table of `apps/web/src/chat/member-row-actions.test.ts`, row for row. */
public class MemberRowActionsTest {
    private val roles = MemberRole.entries

    private fun row(viewer: MemberRole?, rowRole: MemberRole, ownRow: Boolean = false, ended: Boolean = false) =
        memberRowActions(viewer, rowRole, ownRow, ended)

    @Test
    public fun neverOffersAnythingOnTheOwnersRow() {
        for (viewer in roles + listOf<MemberRole?>(null)) {
            val r = row(viewer, MemberRole.OWNER)
            assertEquals(viewer == null, r.remove)
            assertNull(r.adminToggle)
        }
    }

    @Test
    public fun anAdminRemovesPlainMembersOnlyAndNeverChangesRoles() {
        assertEquals(MemberRowActions(MemberRole.OWNER, false, null), row(MemberRole.ADMIN, MemberRole.OWNER))
        assertEquals(MemberRowActions(MemberRole.ADMIN, false, null), row(MemberRole.ADMIN, MemberRole.ADMIN))
        assertEquals(MemberRowActions(null, true, null), row(MemberRole.ADMIN, MemberRole.MEMBER))
    }

    @Test
    public fun theOwnerRemovesAnyoneElseAndMakesOrTakesBackAdmins() {
        assertEquals(MemberRowActions(MemberRole.ADMIN, true, AdminToggle.TAKE), row(MemberRole.OWNER, MemberRole.ADMIN))
        assertEquals(MemberRowActions(null, true, AdminToggle.MAKE), row(MemberRole.OWNER, MemberRole.MEMBER))
    }

    @Test
    public fun aPlainMemberActsOnNoRowButSeesTheMarks() {
        for (r in roles) {
            val out = row(MemberRole.MEMBER, r)
            assertFalse(out.remove)
            assertNull(out.adminToggle)
            assertEquals(if (r == MemberRole.MEMBER) null else r, out.mark)
        }
    }

    @Test
    public fun nobodyActsOnTheirOwnRowAndTheOwnRowKeepsItsMark() {
        for (viewer in roles) {
            val out = row(viewer, viewer, ownRow = true)
            assertFalse(out.remove)
            assertNull(out.adminToggle)
            assertEquals(if (viewer == MemberRole.MEMBER) null else viewer, out.mark)
        }
        assertFalse(row(null, MemberRole.MEMBER, ownRow = true).remove)
    }

    @Test
    public fun aGroupWithNoRoleTableLetsAnyoneRemoveAnyoneElse() {
        for (r in roles) assertEquals(MemberRowActions(null, true, null), row(null, r))
    }

    @Test
    public fun anEndedMembershipShowsAReadOnlyRosterWithoutMarks() {
        for (viewer in roles + listOf<MemberRole?>(null)) {
            for (r in roles) assertEquals(MemberRowActions(null, false, null), row(viewer, r, ended = true))
        }
    }

    @Test
    public fun rowRoleReadsTheOwnerTheAdminsAndEveryoneElse() {
        val a = byteArrayOf(1)
        val b = byteArrayOf(2)
        val c = byteArrayOf(3)
        assertEquals(MemberRole.OWNER, rowRoleOf(a, a, listOf(b)))
        assertEquals(MemberRole.ADMIN, rowRoleOf(b, a, listOf(b)))
        assertEquals(MemberRole.MEMBER, rowRoleOf(c, a, listOf(b)))
        assertEquals(MemberRole.MEMBER, rowRoleOf(a, null, listOf(b)))
    }
}
