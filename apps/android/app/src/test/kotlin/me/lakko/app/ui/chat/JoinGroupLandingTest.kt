// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.ui.chat

import me.lakko.app.chat.ASK_AGAIN_AFTER_MS
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * UX `group-and-channel-privacy.md` 6.2 / `group-invite-page.md` "After
 * `Request to join`" (amended 2026-11-12): the page waits a day with the wait
 * line and no control, then shows "No answer yet." with `Ask again`; reopened
 * for a group already asked it shows the wait; a joined group opens its chat
 * with no `Open chat` step.
 */
class JoinGroupLandingTest {

    private val group = "ab".repeat(32)
    private val sentAt = 1_000_000_000_000L

    @Test
    fun under_a_day_shows_the_wait_line_and_no_control() {
        assertFalse(joinWaitOffersAskAgain(sentAt, sentAt))
        assertFalse(joinWaitOffersAskAgain(sentAt, sentAt + ASK_AGAIN_AFTER_MS - 1))
    }

    @Test
    fun a_day_or_more_offers_ask_again() {
        assertTrue(joinWaitOffersAskAgain(sentAt, sentAt + ASK_AGAIN_AFTER_MS))
        assertTrue(joinWaitOffersAskAgain(sentAt, sentAt + 30L * ASK_AGAIN_AFTER_MS))
    }

    @Test
    fun reopening_for_a_group_already_asked_shows_the_wait_not_the_request_button() {
        val state = joinPageStateOnOpen(holdsGroup = false, sentAtMs = sentAt, groupIdHex = group)
        assertEquals(JoinGroupLandingViewModel.State.Waiting(sentAt), state)
        // Within the day: the wait line alone.
        assertFalse(joinWaitOffersAskAgain((state as JoinGroupLandingViewModel.State.Waiting).sentAtMs, sentAt + 60_000))
    }

    @Test
    fun a_link_for_a_group_never_asked_shows_request_to_join() {
        assertEquals(
            JoinGroupLandingViewModel.State.Ready,
            joinPageStateOnOpen(holdsGroup = false, sentAtMs = null, groupIdHex = group),
        )
    }

    @Test
    fun a_held_group_opens_its_chat_even_with_a_stale_row() {
        val state = joinPageStateOnOpen(holdsGroup = true, sentAtMs = sentAt, groupIdHex = group)
        assertEquals(group, groupToOpenOnJoin(state))
    }

    @Test
    fun joined_navigates_to_the_chat_and_nothing_else_does() {
        assertEquals(group, groupToOpenOnJoin(JoinGroupLandingViewModel.State.Joined(group)))
        assertNull(groupToOpenOnJoin(JoinGroupLandingViewModel.State.Waiting(sentAt)))
        assertNull(groupToOpenOnJoin(JoinGroupLandingViewModel.State.Ready))
        assertNull(groupToOpenOnJoin(JoinGroupLandingViewModel.State.Requesting))
    }
}
