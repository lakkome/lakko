// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * docs/ux/conversation-info.md — one entry point, three destinations.
 *
 * Section 0 records what a live emulator showed on 2026-09-23: a chat with one
 * other person opened the group panel. The first group of tests below is that
 * defect, stated as a rule.
 */
class ConversationInfoTest {

    private val anna = ContactLink(routeKey = "anna-key", groupIdHex = "AA11")
    private val ivan = ContactLink(routeKey = "ivan-key", groupIdHex = "BB22")

    // ---------------------------------------------------------------------
    // Which info opens
    // ---------------------------------------------------------------------

    @Test
    fun `a chat with one other person opens that person, not a group`() {
        val info = conversationInfo(
            kind = ConversationKind.Chat,
            otherMemberCount = 1,
            groupIdHex = "AA11",
            peerRouteKey = "anna-key",
            contacts = listOf(anna, ivan),
        )
        assertEquals(ConversationInfo.Person("anna-key"), info)
    }

    @Test
    fun `a group opens the group`() {
        val info = conversationInfo(
            kind = ConversationKind.Group,
            otherMemberCount = 4,
            groupIdHex = "CC33",
            peerRouteKey = null,
            contacts = listOf(anna, ivan),
        )
        assertEquals(ConversationInfo.Group, info)
    }

    @Test
    fun `a conversation stored as a group opens the group even with two people`() {
        // The new-group flow writes the kind down, so a group that has not
        // grown yet is still a group.
        val info = conversationInfo(
            kind = ConversationKind.Group,
            otherMemberCount = 1,
            groupIdHex = "AA11",
            peerRouteKey = "anna-key",
            contacts = listOf(anna),
        )
        assertEquals(ConversationInfo.Group, info)
    }

    @Test
    fun `a chat that has grown a third person opens the group`() {
        // The persisted kind has not caught up yet; the roster has.
        val info = conversationInfo(
            kind = ConversationKind.Chat,
            otherMemberCount = 2,
            groupIdHex = "AA11",
            peerRouteKey = null,
            contacts = listOf(anna),
        )
        assertEquals(ConversationInfo.Group, info)
    }

    @Test
    fun `the chat is matched before the signature key`() {
        // A leaf key names one of a person's devices (ADR-0050), so a key that
        // matches nobody must not stop the chat's own contact being found.
        val info = conversationInfo(
            kind = ConversationKind.Chat,
            otherMemberCount = 1,
            groupIdHex = "AA11",
            peerRouteKey = "some-other-device-key",
            contacts = listOf(anna, ivan),
        )
        assertEquals(ConversationInfo.Person("anna-key"), info)
    }

    @Test
    fun `the signature key is the fallback when no contact records the chat`() {
        val noChat = ContactLink(routeKey = "anna-key", groupIdHex = null)
        val info = conversationInfo(
            kind = ConversationKind.Chat,
            otherMemberCount = 1,
            groupIdHex = "AA11",
            peerRouteKey = "anna-key",
            contacts = listOf(noChat),
        )
        assertEquals(ConversationInfo.Person("anna-key"), info)
    }

    @Test
    fun `a chat id matches whatever case it is written in`() {
        val info = conversationInfo(
            kind = ConversationKind.Chat,
            otherMemberCount = 1,
            groupIdHex = "aa11",
            peerRouteKey = null,
            contacts = listOf(anna),
        )
        assertEquals(ConversationInfo.Person("anna-key"), info)
    }

    @Test
    fun `a chat that outlived its contact is still a person`() {
        // Removing a contact deliberately leaves the chat behind. It must not
        // fall back to the group panel.
        val info = conversationInfo(
            kind = ConversationKind.Chat,
            otherMemberCount = 1,
            groupIdHex = "ZZ99",
            peerRouteKey = "gone-key",
            contacts = listOf(anna, ivan),
        )
        assertEquals(ConversationInfo.PersonUnknown, info)
    }

    @Test
    fun `no contacts at all still opens a person`() {
        val info = conversationInfo(
            kind = ConversationKind.Chat,
            otherMemberCount = 1,
            groupIdHex = "AA11",
            peerRouteKey = "anna-key",
            contacts = emptyList(),
        )
        assertEquals(ConversationInfo.PersonUnknown, info)
    }

    @Test
    fun `a conversation whose roster is not readable yet still opens a person`() {
        // The count arrives as 0 before the first roster read; a chat is the
        // safe reading, and it is the one the header already takes.
        val info = conversationInfo(
            kind = ConversationKind.Chat,
            otherMemberCount = 0,
            groupIdHex = "AA11",
            peerRouteKey = null,
            contacts = listOf(anna),
        )
        assertEquals(ConversationInfo.Person("anna-key"), info)
    }

    // ---------------------------------------------------------------------
    // What a person's info holds — section 2.1
    // ---------------------------------------------------------------------

    @Test
    fun `a person never shows anything that belongs to a group`() {
        // "Never: a group name, an invite link, roles, ownership, or Leave
        // group." Asserted against the classification on the enum, so a group
        // section added later is covered without editing this test.
        for (origin in InfoOrigin.entries) {
            for (isContact in listOf(true, false)) {
                for (ended in listOf(true, false)) {
                    val sections = personInfoSections(origin, isContact, ended)
                    val offending = sections.filter { it.groupOnly }
                    assertTrue(
                        "$origin isContact=$isContact ended=$ended leaked $offending",
                        offending.isEmpty(),
                    )
                }
            }
        }
    }

    @Test
    fun `at least one section is classified group-only`() {
        // Guards the test above from passing because the classification is
        // empty rather than because the rule holds.
        assertTrue(InfoSection.entries.any { it.groupOnly })
    }

    @Test
    fun `a contact shows the safety number wherever it was opened from`() {
        for (origin in InfoOrigin.entries) {
            assertTrue(
                origin.toString(),
                InfoSection.SafetyNumber in personInfoSections(origin, isContact = true),
            )
        }
    }

    @Test
    fun `opened from the conversation, there is no Message button`() {
        val sections = personInfoSections(InfoOrigin.Conversation, isContact = true)
        assertFalse(InfoSection.Message in sections)
        assertTrue(InfoSection.Call in sections)
    }

    @Test
    fun `opened from anywhere else, Message is offered`() {
        assertTrue(
            InfoSection.Message in personInfoSections(InfoOrigin.Elsewhere, isContact = true),
        )
    }

    @Test
    fun `a person who is not a contact offers no rename and no removal`() {
        val sections = personInfoSections(InfoOrigin.Conversation, isContact = false)
        assertFalse(InfoSection.RenameContact in sections)
        assertFalse(InfoSection.RemoveContact in sections)
        assertFalse(InfoSection.SafetyNumber in sections)
        assertTrue(InfoSection.Call in sections)
    }

    // ---------------------------------------------------------------------
    // Membership ended — section 4
    // ---------------------------------------------------------------------

    @Test
    fun `an ended conversation offers no call`() {
        // The per-call key comes from a group this device is no longer in, so
        // a call control would assert a live session there is not.
        val sections = personInfoSections(
            InfoOrigin.Conversation,
            isContact = true,
            membershipEnded = true,
        )
        assertFalse(InfoSection.Call in sections)
    }

    @Test
    fun `an ended conversation can still be deleted`() {
        // ADR-0045 UX 6.2.4 — the one thing still open. It used to live in the
        // group panel, which a one-to-one no longer opens.
        val sections = personInfoSections(
            InfoOrigin.Conversation,
            isContact = true,
            membershipEnded = true,
        )
        assertTrue(InfoSection.DeleteChat in sections)
    }

    @Test
    fun `a live conversation offers no delete`() {
        assertFalse(
            InfoSection.DeleteChat in
                personInfoSections(InfoOrigin.Conversation, isContact = true),
        )
    }

    @Test
    fun `an ended conversation still holds everything about the person`() {
        // The conversation ended; they did not.
        val sections = personInfoSections(
            InfoOrigin.Conversation,
            isContact = true,
            membershipEnded = true,
        )
        assertTrue(InfoSection.SafetyNumber in sections)
        assertTrue(InfoSection.RenameContact in sections)
        assertTrue(InfoSection.RemoveContact in sections)
    }

    @Test
    fun `an ended conversation with no contact record still offers the delete`() {
        val sections = personInfoSections(
            InfoOrigin.Conversation,
            isContact = false,
            membershipEnded = true,
        )
        assertTrue(InfoSection.DeleteChat in sections)
        assertFalse(InfoSection.Call in sections)
    }

    // ---------------------------------------------------------------------
    // What a channel's info holds — section 2.3
    // ---------------------------------------------------------------------

    @Test
    fun `a channel never shows anything that belongs to a group`() {
        for (isPublisher in listOf(true, false)) {
            for (known in listOf(true, false)) {
                val sections = channelInfoSections(isPublisher, known)
                assertTrue(
                    "publisher=$isPublisher known=$known",
                    sections.none { it.groupOnly },
                )
            }
        }
    }

    @Test
    fun `a reader gets the handle and the description and no manage action`() {
        val sections = channelInfoSections(isPublisher = false, subscriberCountKnown = false)
        assertTrue(InfoSection.ChannelHandle in sections)
        assertTrue(InfoSection.ChannelDescription in sections)
        assertFalse(InfoSection.ManageChannel in sections)
    }

    @Test
    fun `the subscriber count shows only where the client has one`() {
        assertFalse(
            InfoSection.SubscriberCount in
                channelInfoSections(isPublisher = false, subscriberCountKnown = false),
        )
        assertTrue(
            InfoSection.SubscriberCount in
                channelInfoSections(isPublisher = true, subscriberCountKnown = true),
        )
    }

    @Test
    fun `a publisher gets the manage action`() {
        assertTrue(
            InfoSection.ManageChannel in
                channelInfoSections(isPublisher = true, subscriberCountKnown = true),
        )
    }
}
