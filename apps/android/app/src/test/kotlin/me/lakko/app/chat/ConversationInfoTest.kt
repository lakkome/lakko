// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * docs/ux/conversation-info.md — which of the three destinations a
 * conversation's header opens, and whose contact record it belongs to.
 *
 * Section 0 records what a live emulator showed on 2026-09-23: a chat with one
 * other person opened the group panel. The tests below are that defect, stated
 * as a rule.
 *
 * ## What is no longer asserted here, and why it is not a loss
 *
 * This file used to also assert WHAT each destination holds — the sections of
 * a person's info and of a channel's, the `Never` list of section 2.1, the
 * subscriber count, the ended-membership turnover. That rule now lives once,
 * in `crates/lakko-ui-rules/src/info.rs`, where it is asserted exhaustively
 * across every combination rather than at the examples a hand-written Kotlin
 * copy happened to pick — which is stronger than anything this file had, and
 * is what stops the drift that gave the same paragraph three different
 * answers on three clients.
 *
 * It cannot be re-asserted from here. `personInfoSections` and
 * `channelInfoSections` are now UniFFI wrappers over that crate, and a JVM
 * unit test does not load the native library, so calling one would only prove
 * the library is absent — which it is. (`CallInviteLinkTest` makes the same
 * argument about the call-invite wrappers, for the same reason.)
 *
 * What stays is the matching rule, which is genuinely this client's: it reads
 * this device's own contact list and the chat it recorded, and the core holds
 * neither.
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
}
