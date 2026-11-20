// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.lakko_ffi_uniffi.ChannelSubscriberList
import uniffi.lakko_ffi_uniffi.ChannelSubscriberPerson

/**
 * Sections 15.1 / 7.3 — base walk 2026-11-13 (scenario 27): a private
 * channel's owner could not tell whom `Remove` took. The rule itself is
 * `lakko_ui_rules::subscribers`, tested there and across the web and iOS
 * boundaries; it is an FFI call the host JVM cannot load, so this suite
 * covers what the client passes in and how it maps the answer back, and the
 * reader-card trigger (the Pixel never uploaded its card).
 */
class ChannelSubscriberListViewTest {
    private val iphonePk = "aa".repeat(32)
    private val pixelPk = "bb".repeat(32)

    private fun member(key: String, name: String, pks: List<String>, named: Boolean, isSelf: Boolean = false) =
        ChannelMemberRow(key = key, name = name, username = null, isSelf = isSelf, subscriberPksHex = pks, named = named)

    @Test
    fun aNamelessCardIsPassedAsUnnamedAndEveryDeviceAsACard() {
        val input = ChannelSubscriberListView.input(
            listOf(
                member("pixel", "Pixel Four", listOf(pixelPk, "CC".repeat(32)), named = true),
                member("iphone", "Member", listOf(iphonePk), named = false),
                member("self", "You", listOf("dd".repeat(32)), named = true, isSelf = true),
            ),
        )
        assertEquals(listOf(pixelPk, "cc".repeat(32), iphonePk, "dd".repeat(32)), input.cards.map { it.subscriberKey })
        assertEquals(listOf("pixel", "pixel", "iphone", "self"), input.cards.map { it.personKey })
        assertEquals(listOf(true, true, false, true), input.cards.map { it.named })
        assertEquals(listOf(false, false, false, true), input.cards.map { it.isSelf })
    }

    @Test
    fun theAnswerMapsBackToTheRowsAndKeys() {
        val pixel = member("pixel", "Pixel Four", listOf(pixelPk), named = true)
        val iphone = member("iphone", "Member", listOf(iphonePk), named = false)
        val input = ChannelSubscriberListView.input(listOf(pixel, iphone))
        val listed = listOf(ChannelSubscriberRows.Row(iphonePk, null), ChannelSubscriberRows.Row(pixelPk, null))
        val view = ChannelSubscriberListView.view(
            input,
            listed,
            ChannelSubscriberList(
                people = listOf(ChannelSubscriberPerson(cards = listOf(0u), removable = true)),
                others = listOf(0u),
                othersCount = 1uL,
            ),
        )
        assertEquals(listOf("Pixel Four"), view.people.map { it.row.name })
        assertEquals(listOf(pixelPk), view.people.single().pubkeysHex)
        assertTrue(view.people.single().removable)
        assertEquals(listOf(iphonePk), view.others.map { it.pubkeyHex })
        assertEquals(1L, view.othersCount)
    }

    @Test
    fun theReaderCardIsDueWhenASeedFirstArrivesAndAfterEachRotation() {
        // Not let in yet: no seed held, nothing to upload.
        assertFalse(ChannelSubscriberListView.readerCardDue(null, null))
        // Let in: the first seed arrives.
        assertTrue(ChannelSubscriberListView.readerCardDue(null, 3u))
        assertFalse(ChannelSubscriberListView.readerCardDue(3u, 3u))
        assertTrue(ChannelSubscriberListView.readerCardDue(3u, 4u))
        assertFalse(ChannelSubscriberListView.readerCardDue(4u, null))
    }

    @Test
    fun thisDevicesOwnKeyIsPassedAsTheViewersOwnCard() {
        val own = "EE".repeat(32)
        val input = ChannelSubscriberListView.input(
            listOf(member("pixel", "Pixel Four", listOf(pixelPk), named = true)),
            ownKeyHex = own,
        )
        assertEquals(listOf(own.lowercase(), pixelPk), input.cards.map { it.subscriberKey })
        assertEquals(listOf(true, false), input.cards.map { it.isSelf })
        // Already among the cards: not added twice.
        val again = ChannelSubscriberListView.input(
            listOf(member("self", "You", listOf(own.lowercase()), named = true, isSelf = true)),
            ownKeyHex = own,
        )
        assertEquals(1, again.cards.size)
    }

    @Test
    fun aLinksLabelNamesTheRowAndAnEmptyOneReadsAsAnInvitation() {
        val labels = mapOf("ab".repeat(32) to "Friends", "cd".repeat(32) to "  ")
        assertEquals("Friends", ChannelSubscriberRows.linkLabelOf("AB".repeat(32), labels))
        assertEquals(null, ChannelSubscriberRows.linkLabelOf("cd".repeat(32), labels))
        assertEquals(null, ChannelSubscriberRows.linkLabelOf("ef".repeat(32), labels))
        assertEquals(null, ChannelSubscriberRows.linkLabelOf(null, labels))
    }

    @Test
    fun goingPrivateKeepsEveryoneSubscribedAndNoTicketAloneLetsNobodyIn() {
        fun v(row: ChannelSubscriberRows.Row, live: Boolean, mark: Long, set: Boolean, own: String? = null) =
            ChannelSubscriberRows.letInVerdict(row, live, mark, set, own)
        val reader = ChannelSubscriberRows.Row("aa".repeat(32), inviteKeyHex = null, seq = 4)
        val stranger = ChannelSubscriberRows.Row("bb".repeat(32), inviteKeyHex = null, seq = 9)
        val invited = ChannelSubscriberRows.Row("cc".repeat(32), inviteKeyHex = "dd".repeat(32), seq = 10)
        // UX 4.1 — a row from before the channel went private stays.
        assertEquals(ChannelSubscriberRows.LetIn.ADMIT, v(reader, false, 5L, true))
        // Above the mark, no valid ticket: removed on the device that set it.
        assertEquals(ChannelSubscriberRows.LetIn.REMOVE, v(stranger, false, 5L, true))
        assertEquals(ChannelSubscriberRows.LetIn.ADMIT, v(invited, true, 5L, true))
        // A device that did not set the mark: neither let in nor removed.
        assertEquals(ChannelSubscriberRows.LetIn.LEAVE, v(stranger, false, 0L, false))
        assertEquals(ChannelSubscriberRows.LetIn.ADMIT, v(invited, true, 0L, false))
        // This device's own card key is never removed.
        assertEquals(ChannelSubscriberRows.LetIn.LEAVE, v(stranger, false, 5L, true, "BB".repeat(32)))
    }

    @Test
    fun theMarkMovesOnlyPastRowsLetInWhereThisDeviceDidNotSetIt() {
        val judged = listOf(
            ChannelSubscriberRows.Row("aa".repeat(32), null, seq = 7) to ChannelSubscriberRows.LetIn.ADMIT,
            ChannelSubscriberRows.Row("bb".repeat(32), null, seq = 9) to ChannelSubscriberRows.LetIn.LEAVE,
            ChannelSubscriberRows.Row("cc".repeat(32), null, seq = 12) to ChannelSubscriberRows.LetIn.ADMIT,
        )
        assertEquals(12L, ChannelSubscriberRows.advancedMark(5L, true, judged))
        assertEquals(7L, ChannelSubscriberRows.advancedMark(5L, false, judged))
        assertEquals(20L, ChannelSubscriberRows.advancedMark(20L, true, judged))
        assertEquals(5L, ChannelSubscriberRows.advancedMark(5L, true, emptyList()))
    }

    /** The rotation reseal resealed the new seed to EVERY listed key,
     *  including a row with no valid ticket a server could have added. */
    @Test
    fun aPrivateRotationResealsOnlyToTheRowsLetIn() {
        assertEquals(8L, ChannelReseal.resealThrough(8L))
        // A device that set no mark: the readers only, never every row.
        assertEquals(0L, ChannelReseal.resealThrough(0L))
        val rows = listOf(
            SubscriberRow(3, "aa".repeat(32), null),
            SubscriberRow(8, "bb".repeat(32), ByteArray(1)),
            SubscriberRow(9, "cc".repeat(32), null),
        )
        assertEquals(listOf("aa".repeat(32), "bb".repeat(32)), ChannelReseal.resealKeys(rows, 8L))
    }

    @Test
    fun aPublisherRegistersACardKeyOnlyInAChannelKnownToBePublic() {
        assertTrue(publisherMayRegisterCardKey(true))
        assertFalse(publisherMayRegisterCardKey(false))
        assertFalse(publisherMayRegisterCardKey(null))
    }
}
