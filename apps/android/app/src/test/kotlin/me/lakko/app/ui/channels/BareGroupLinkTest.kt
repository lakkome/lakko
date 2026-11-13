// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import kotlinx.coroutines.runBlocking
import me.lakko.app.contacts.LakkoLink
import me.lakko.app.l10n.EnglishForTests
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * Base scenario 29 (2026-11-13): the Chats row of an older message whose text
 * is a bare `/g/` link (no card stored) showed the link with its secret. The
 * rule is the web's (`channel-invite-preview.ts`).
 */
class BareGroupLinkTest {
    private val strings = EnglishForTests.install()
    private val origin = "https://test.lakko.me"
    private val link = "https://test.lakko.me/g/AbCdEfGh#0123456789abcdef"
    private val bare = "https://test.lakko.me/g/AbCdEfGh"

    @Test
    fun `only a whole group link of this deployment is one`() {
        assertEquals(link, bareGroupLinkOf(" $link\n", origin))
        assertNull(bareGroupLinkOf("see $link", origin))
        assertNull(bareGroupLinkOf("https://other.example/g/AbCdEfGh#s", origin))
        assertNull(bareGroupLinkOf("https://test.lakko.me/g/AbCdEfGh", origin))
        assertNull(bareGroupLinkOf("https://test.lakko.me/c/AbCdEfGh#s", origin))
        assertEquals("https://other.example/g/AbCdEfGh#s", bareGroupLinkOf("https://other.example/g/AbCdEfGh#s", null))
    }

    @Test
    fun `the row never shows the secret`() {
        assertEquals(bare, bareGroupLinkRowLine(link, null, origin, strings))
        assertEquals(bare, bareGroupLinkRowLine(link, BareLinkOpening.Offline, origin, strings))
        assertEquals(bare, bareGroupLinkRowLine(link, BareLinkOpening.NotChannel, origin, strings))
        assertEquals(strings["invite.link.invalid"], bareGroupLinkRowLine(link, BareLinkOpening.Dead, origin, strings))
        assertEquals(
            "Invitation to News",
            bareGroupLinkRowLine(link, BareLinkOpening.Channel(ChannelInviteSummary("News", publisher = false)), origin, strings),
        )
        assertNull(bareGroupLinkRowLine("hello", null, origin, strings))
    }

    @Test
    fun `the bubble shows the link only once it opened to a group`() {
        assertEquals(bare, bareGroupLinkBubbleWords(link, null, strings))
        assertEquals(bare, bareGroupLinkBubbleWords(link, BareLinkOpening.Offline, strings))
        assertEquals(strings["invite.link.invalid"], bareGroupLinkBubbleWords(link, BareLinkOpening.Dead, strings))
        assertNull(bareGroupLinkBubbleWords(link, BareLinkOpening.NotChannel, strings))
    }

    @Test
    fun `a notification for a bare link carries no secret`() {
        assertEquals(bare, invitationAwarePreviewText(link, null, strings) { error("not called") })
        assertEquals("hello", invitationAwarePreviewText("hello", null, strings) { error("not called") })
    }

    private fun actions(inspect: suspend (LakkoLink.Group) -> ChannelInviteFacts?) = object : ChannelInviteActions {
        override suspend fun inspect(link: LakkoLink.Group): ChannelInviteFacts? = inspect(link)
        override fun isFollowed(channelIdHex: String): Boolean = false
        override suspend fun subscribe(facts: ChannelInviteFacts) = Unit
        override fun openChannel(channelIdHex: String) = Unit
        override suspend fun acceptPublisher(
            facts: ChannelInviteFacts,
            link: LakkoLink.Group,
            onStatus: (me.lakko.app.l10n.UiText?) -> Unit,
        ): String? = null
    }

    @Test
    fun `no answer from the ledger is offline, anything else gone`() = runBlocking {
        val group = LakkoLink.Group("AbCdEfGh", "0123456789abcdef")
        val offline = actions { throw RuntimeException("fetch", me.lakko.app.chat.InviteLinkUnreachable(RuntimeException("io"))) }
        assertEquals(BareLinkOpening.Offline, offline.openBareLink(group))
        val gone = actions { throw IllegalStateException("404") }
        assertEquals(BareLinkOpening.Dead, gone.openBareLink(group))
        val group2 = actions { null }
        assertEquals(BareLinkOpening.NotChannel, group2.openBareLink(group))
    }

    // Follow-up to scenario 29: a reply quote (and the pinned bar, which reads
    // it) and a saved snapshot of a bare link take the row's line.
    private fun message(text: String) = me.lakko.app.chat.ChatMessage(
        messageId = 42L,
        direction = me.lakko.app.chat.Direction.Incoming,
        text = text,
        attachments = emptyList(),
        createdAt = "1970-01-01T00:00:00Z",
        authorKey = "Ym9i",
    )

    @Test
    fun `snapshots of a bare link never carry the secret`() {
        val line: (Map<String, BareLinkOpening>) -> (String) -> String? = { openings ->
            { text -> bareGroupLinkSnapshotLine(text, origin, openings, strings) }
        }
        val reply = me.lakko.app.chat.replySnapshotFor(
            message(link), "Anna", inviteLine = { null }, bareLinkLine = line(emptyMap()),
        )
        assertEquals(bare, reply.excerpt)
        val saved = me.lakko.app.chat.savedExcerpt(
            message(link), inviteLine = { null }, bareLinkLine = line(mapOf(link to BareLinkOpening.Dead)),
        )
        assertEquals(strings["invite.link.invalid"], saved)
        val channel = me.lakko.app.chat.replySnapshotFor(
            message(link), "Anna", inviteLine = { null },
            bareLinkLine = line(mapOf(link to BareLinkOpening.Channel(ChannelInviteSummary("Walk", publisher = true)))),
        )
        assertEquals("Invitation to publish in Walk", channel.excerpt)
        val plain = me.lakko.app.chat.replySnapshotFor(message("hello"), "Anna", inviteLine = { null }, bareLinkLine = line(emptyMap()))
        assertEquals("hello", plain.excerpt)
    }

    // Follow-up to scenario 29: the bubble's card lookup kept one network
    // failure for the rest of the run; now it asks again until the ledger
    // answers, and only an answer is taken.
    @Test
    fun `a ledger that does not answer is asked again, an answer ends it`() = runBlocking {
        var calls = 0
        val waits = mutableListOf<Int>()
        val unreachable = RuntimeException("fetch", me.lakko.app.chat.InviteLinkUnreachable(RuntimeException("io")))
        val out = inspectUntilAnswered(
            inspect = {
                calls += 1
                if (calls < 3) throw unreachable
                null
            },
            wait = { waits += it },
        )
        assertNull(out)
        assertEquals(3, calls)
        assertEquals(listOf(0, 1), waits)
        // A link that is gone is an answer: no retry.
        calls = 0
        inspectUntilAnswered(inspect = { calls += 1; throw IllegalStateException("404") }, wait = { error("no wait") })
        assertEquals(1, calls)
        assertEquals(listOf(5_000L, 10_000L, 20_000L, 40_000L, 60_000L, 60_000L), (0..5).map(::inspectRetryDelayMs))
    }

    // Seen live 2026-11-13: an old publisher link carrying no channel name
    // read "Invitation to publish in " while the device held the channel.
    @Test
    fun `an invitation with no name reads by the held name`() {
        val id = "ef".repeat(16)
        assertEquals("Walk", inviteChannelName(" Walk ", id, held = { "Other" }, strings = strings))
        assertEquals("Walk 22 channel", inviteChannelName("", id, held = { "Walk 22 channel" }, strings = strings))
        assertEquals(strings["channels.fallback_name"], inviteChannelName(null, id, held = { null }, strings = strings))
        val actions = actions { null }
        val holding = object : ChannelInviteActions by actions {
            override fun heldChannelName(channelIdHex: String): String? =
                if (channelIdHex == id) "Walk 22 channel" else null
        }
        val before = ChannelInviteActionsHolder.actions
        ChannelInviteActionsHolder.actions = holding
        try {
            assertEquals("Walk 22 channel", inviteChannelName("  ", id, strings = strings))
        } finally {
            ChannelInviteActionsHolder.actions = before
        }
    }
}
