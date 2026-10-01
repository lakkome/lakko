// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
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
            groupIdHex = "CC33",
            peerRouteKey = null,
            contacts = listOf(anna, ivan),
        )
        assertEquals(ConversationInfo.Group, info)
    }

    @Test
    fun `a two-person group opens the group`() {
        // ADR-0062 — the kind is read from the group, never from the people
        // count, so a group that has not grown yet is still a group.
        val info = conversationInfo(
            kind = ConversationKind.Group,
            groupIdHex = "AA11",
            peerRouteKey = "anna-key",
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
            groupIdHex = "aa11",
            peerRouteKey = null,
            contacts = listOf(anna),
        )
        assertEquals(ConversationInfo.Person("anna-key"), info)
    }

    @Test
    fun `a one-to-one whose record cannot be resolved is a person with no card`() {
        // A roster that cannot be read: still a person, never the group
        // panel, and no card to open (every peer is a contact by construction,
        // so this is the only way the key is absent).
        val info = conversationInfo(
            kind = ConversationKind.Chat,
            groupIdHex = "ZZ99",
            peerRouteKey = "gone-key",
            contacts = listOf(anna, ivan),
        )
        assertEquals(ConversationInfo.Person(contactKey = null), info)
    }

    @Test
    fun `no contacts at all is still a person, with no card`() {
        val info = conversationInfo(
            kind = ConversationKind.Chat,
            groupIdHex = "AA11",
            peerRouteKey = "anna-key",
            contacts = emptyList(),
        )
        assertEquals(ConversationInfo.Person(contactKey = null), info)
    }

    // ---------------------------------------------------------------------
    // How the header presents it — section 3, "Except a person"
    // ---------------------------------------------------------------------

    @Test
    fun `a person who is a contact is pushed as the contact screen, not a layer`() {
        // Seen live on 2026-09-26: the header opened the same person in a
        // sheet, without `Message`, while the contacts list pushed a screen —
        // two cards for one contact. There is one card, and it is the screen.
        assertEquals(
            HeaderInfoPresentation.ContactScreen("anna-key"),
            headerInfoPresentation(ConversationInfo.Person("anna-key")),
        )
    }

    @Test
    fun `a group is a layer over the conversation`() {
        assertEquals(HeaderInfoPresentation.GroupLayer, headerInfoPresentation(ConversationInfo.Group))
    }

    @Test
    fun `a person with no record gives the header nothing to open`() {
        // docs/ux/conversation-info.md section 2.1 — not a state with a
        // screen: the tap does nothing, and nothing is said.
        assertNull(headerInfoPresentation(ConversationInfo.Person(contactKey = null)))
    }

    @Test
    fun `a conversation whose roster is not readable yet still opens a person`() {
        // A one-to-one whose roster has not been read yet: the kind comes
        // from the group, so the header still opens the person the chat
        // records.
        val info = conversationInfo(
            kind = ConversationKind.Chat,
            groupIdHex = "AA11",
            peerRouteKey = null,
            contacts = listOf(anna),
        )
        assertEquals(ConversationInfo.Person("anna-key"), info)
    }
}
