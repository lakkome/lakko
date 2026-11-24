// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.ui.chat.JoinGroupLandingViewModel
import me.lakko.app.ui.chat.joinPageStateOnOpen
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Seen live 2026-11-23: a member left a group and opened its invite link
 * again; the page showed "Request sent. An admin will see it..." and nothing
 * reached the admin, because the `Requests sent` row of the request that had
 * led to the group was never dropped (only a screen asking `holdsGroup` while
 * the group was held dropped it), and the page took it for a request waiting.
 * The row now goes when the group lands and when the membership ends.
 */
class RequestsSentForgetTest {

    private val group = "ab".repeat(32)
    private val other = "cd".repeat(32)

    private fun row(id: String, at: Long) = SentRequest(idHex = id, name = "", handle = "", sentAtMs = at)

    @Test
    fun the_old_row_made_the_page_wait_without_a_request() {
        // The reproduction: the row from the earlier, approved request.
        val rows = listOf(row(group, 1_000L))
        val state = joinPageStateOnOpen(holdsGroup = false, sentAtMs = rows.first().sentAtMs, groupIdHex = group)
        assertTrue(state is JoinGroupLandingViewModel.State.Waiting)
    }

    @Test
    fun after_the_join_and_the_leave_the_page_offers_request_to_join() {
        var rows = listOf(row(group, 1_000L), row(other, 1_500L))
        // The group lands at 2 000: the request is answered.
        rows = sentRequestsAfter(rows, group, upToMs = 2_000L)
        // The person leaves at 3 000.
        rows = sentRequestsAfter(rows, group, upToMs = 3_000L)
        val sent = rows.firstOrNull { it.idHex == group }?.sentAtMs
        assertEquals(
            JoinGroupLandingViewModel.State.Ready,
            joinPageStateOnOpen(holdsGroup = false, sentAtMs = sent, groupIdHex = group),
        )
        // Another group's row is untouched.
        assertEquals(listOf(row(other, 1_500L)), rows)
    }

    @Test
    fun a_request_sent_after_the_end_was_recorded_stays() {
        val rows = listOf(row(group, 5_000L))
        // An end recorded at 4 000 and carried out late does not take the
        // request the person sent at 5 000.
        assertEquals(rows, sentRequestsAfter(rows, group, upToMs = 4_000L))
    }

    @Test
    fun the_id_is_matched_without_case() {
        assertTrue(sentRequestsAfter(listOf(row(group, 1L)), group.uppercase(), upToMs = 1L).isEmpty())
    }
}
