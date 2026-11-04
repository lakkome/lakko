// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.push

import me.lakko.app.l10n.EnglishForTests
import me.lakko.app.profile.ResolvedName
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test

/** A reaction notification names the reactor by the name stored on this
 *  device. With none there is no reaction line: the wake shows the generic
 *  banner, never "Someone", "Member" or the conversation's name. */
class ReactionNotificationNameTest {

    private val own = "OWN="
    private val anna = "ANNA="
    private val annaLeaf = "ANNALEAF="

    @Before
    fun english() {
        EnglishForTests.install()
    }

    private fun reaction(senderName: String?) = ProcessedRow(
        kind = ProcessedKind.REACTION,
        text = "",
        attachmentCount = 0,
        senderName = senderName,
        senderAccountB64 = anna,
        reactionAdd = true,
        reactionEmoji = "👍",
        reactionTargetAuthorB64 = own,
    )

    private fun named(name: String) =
        ResolvedName(primary = name, verifiedUsername = null, isPlaceholder = false, theirName = null, usernameNotVerified = false, fingerprint = "")

    private val placeholder =
        ResolvedName(primary = "Member", verifiedUsername = null, isPlaceholder = true, theirName = null, usernameNotVerified = false, fingerprint = "")

    @Test
    fun `a known reactor is named and an unnamed one gives no line`() {
        val body = reactionNotificationBody(reaction("Anna"), own, enabled = { true }, muted = { false })!!
        assertEquals("Anna reacted 👍 to your message", body)
        val shape = notificationShape(me.lakko.app.chat.ConversationKind.Group, "Test VG", "Anna", body, prefixSender = false)
        assertEquals("Test VG" to "Anna reacted 👍 to your message", shape)
        assertNoPlaceholderWord(shape?.first, shape?.second)

        val unnamed = reactionNotificationBody(reaction(null), own, enabled = { true }, muted = { false })!!
        assertNoPlaceholderWord(unnamed)
        assertNull(notificationShape(me.lakko.app.chat.ConversationKind.Group, "Test VG", null, unnamed, prefixSender = false))
    }

    @Test
    fun `the background name is looked up under the person key, then the leaf key, else none`() {
        val asked = mutableListOf<String>()
        val byPerson = { key: String ->
            asked += key
            if (key == anna) named("Anna") else placeholder
        }
        assertEquals("Anna", backgroundSenderName(anna, annaLeaf, byPerson))
        assertEquals(listOf(anna), asked)

        val byLeaf = { key: String -> if (key == annaLeaf) named("Anna") else placeholder }
        assertEquals("Anna", backgroundSenderName(anna, annaLeaf, byLeaf))

        assertNull(backgroundSenderName(anna, annaLeaf) { placeholder })
    }
}
