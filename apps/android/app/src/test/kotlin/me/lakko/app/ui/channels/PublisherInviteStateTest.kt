// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.channels

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * UX section 15.2 / 15.2.2 — a publisher invitation on the card and the
 * `/g/` page: Accept once, the waiting line, `Ask again` after seven days,
 * the invalid line when the link stopped working, `Open` once a publisher.
 */
class PublisherInviteStateTest {

    private val now = 1_000_000_000_000L

    private fun control(
        isPublisher: Boolean = false,
        sentAt: Long? = null,
        sending: Boolean = false,
        linkDead: Boolean = false,
    ) = publisherInviteControl(isPublisher, sentAt, now, sending, linkDead)

    @Test
    fun nothing_sent_offers_accept() {
        assertEquals(PublisherInviteControl.ACCEPT, control())
    }

    @Test
    fun a_press_in_flight_shows_the_disabled_waiting_button() {
        assertEquals(PublisherInviteControl.SENDING, control(sending = true))
        assertEquals(PublisherInviteControl.SENDING, control(sentAt = now - PUBLISHER_ASK_AGAIN_AFTER_MS, sending = true))
    }

    @Test
    fun a_request_under_seven_days_waits_with_no_button() {
        assertEquals(PublisherInviteControl.WAITING, control(sentAt = now))
        assertEquals(PublisherInviteControl.WAITING, control(sentAt = now - PUBLISHER_ASK_AGAIN_AFTER_MS + 1))
    }

    @Test
    fun seven_days_or_more_offers_ask_again() {
        assertEquals(PublisherInviteControl.ASK_AGAIN, control(sentAt = now - PUBLISHER_ASK_AGAIN_AFTER_MS))
        assertEquals(PublisherInviteControl.ASK_AGAIN, control(sentAt = now - 30L * PUBLISHER_ASK_AGAIN_AFTER_MS))
    }

    @Test
    fun a_dead_link_shows_the_invalid_line_instead_of_the_button() {
        assertEquals(PublisherInviteControl.LINK_INVALID, control(linkDead = true))
        assertEquals(PublisherInviteControl.LINK_INVALID, control(sentAt = now - PUBLISHER_ASK_AGAIN_AFTER_MS, linkDead = true))
    }

    @Test
    fun a_publisher_gets_open() {
        assertEquals(PublisherInviteControl.OPEN, control(isPublisher = true, sentAt = now))
    }

    @Test
    fun failures_are_classified_through_the_cause_chain() {
        assertEquals(
            PublisherAcceptFailure.LINK_DEAD,
            publisherAcceptFailure(RuntimeException(me.lakko.app.channels.CoPublisher.CoPublisherException.NotACoPublisherLink("x"))),
        )
        assertEquals(PublisherAcceptFailure.OFFLINE, publisherAcceptFailure(java.io.IOException("down")))
        assertEquals(PublisherAcceptFailure.OTHER, publisherAcceptFailure(IllegalStateException("other")))
    }
}
