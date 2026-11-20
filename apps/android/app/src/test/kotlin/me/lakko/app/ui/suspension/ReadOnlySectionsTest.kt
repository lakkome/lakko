// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.suspension

import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.lakko_ffi_uniffi.InfoSection

/** ADR-0099 section 4 - a suspended account is offered no calls and no way
 *  to add people; the rest of the info keeps its order. */
class ReadOnlySectionsTest {

    private val all = listOf(
        InfoSection.MESSAGE,
        InfoSection.CALL,
        InfoSection.ADD_CONTACT,
        InfoSection.MEMBER_LIST,
        InfoSection.ADD_MEMBER,
        InfoSection.INVITE_LINK,
        InfoSection.INVITE_PEOPLE,
        InfoSection.REPORT,
        InfoSection.LEAVE_GROUP,
    )

    @Test
    fun `suspended - calls and adding people go, the rest stays in order`() {
        assertEquals(
            listOf(InfoSection.MESSAGE, InfoSection.MEMBER_LIST, InfoSection.REPORT, InfoSection.LEAVE_GROUP),
            readOnlySections(all, suspended = true),
        )
    }

    @Test
    fun `suspended - the message menu offers nothing that sends`() {
        val actions = me.lakko.app.ui.chat.messageActionsFor(
            isOwn = true,
            deleted = false,
            text = "hello",
            menuAvailable = true,
            pinLabel = "Pin",
            suspended = true,
        )
        assertEquals(false, actions.canReply)
        assertEquals(false, actions.canForward)
        assertEquals(false, actions.canEdit)
        assertEquals(false, actions.canDelete)
        // What only reads stays.
        assertEquals(true, actions.canCopy)
        assertEquals(true, actions.canSave)
        val normal = me.lakko.app.ui.chat.messageActionsFor(
            isOwn = true,
            deleted = false,
            text = "hello",
            menuAvailable = true,
            pinLabel = "Pin",
            suspended = false,
        )
        assertEquals(true, normal.canReply)
        assertEquals(true, normal.canForward)
    }

    @Test
    fun `suspended - the pin menu offers only the personal pin`() {
        val offered = me.lakko.app.chat.sharedPinOffered(mayShared = true, suspended = true)
        assertEquals(false, offered)
        // Nothing pinned: `Pin for me` alone.
        assertEquals(
            listOf(me.lakko.app.chat.PinAction.PIN_ME),
            me.lakko.app.chat.pinMenuFor(sharedPinned = false, personalPinned = false, mayShared = offered).actions,
        )
        // Pinned for everyone by someone, not for me: still `Pin for me` alone,
        // no shared Unpin.
        assertEquals(
            listOf(me.lakko.app.chat.PinAction.PIN_ME),
            me.lakko.app.chat.pinMenuFor(sharedPinned = true, personalPinned = false, mayShared = offered).actions,
        )
        // Pinned both ways: only the personal Unpin.
        assertEquals(
            listOf(me.lakko.app.chat.PinAction.UNPIN_ME),
            me.lakko.app.chat.pinMenuFor(sharedPinned = true, personalPinned = true, mayShared = offered).actions,
        )
        assertEquals(true, me.lakko.app.chat.sharedPinOffered(mayShared = true, suspended = false))
    }

    @Test
    fun `not suspended - unchanged`() {
        assertEquals(all, readOnlySections(all, suspended = false))
    }
}
