// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import java.util.Base64
import kotlinx.coroutines.delay
import kotlinx.coroutines.test.runTest
import me.lakko.app.moderation.AccountStanding
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiLinkAnswer

/**
 * ADR-0089 step 4 — the inbox poll screens a fetched batch before anything is
 * stored or decided. A request that answers one of this account's invite
 * links is accepted with no card once decided, so a verified suspension must
 * drop it here, before the poll keeps it undecided.
 */
class RequestScreeningTest {

    private fun request(id: Long, keyByte: Int, link: Boolean) = IncomingContactRequest(
        messageId = id,
        fromPubkey = ByteArray(32) { keyByte.toByte() },
        fromAccountId = null,
        fromPseudonym = ByteArray(32),
        validAfterMs = 0uL,
        intro = null,
        createdAt = "2026-11-20T10:00:00Z",
        linkAnswer = if (link) FfiLinkAnswer(linkId = ByteArray(16) { 1 }, linkSecret = ByteArray(12) { 2 }) else null,
    )

    private fun b64(r: IncomingContactRequest): String = Base64.getEncoder().encodeToString(r.fromPubkey)

    @Test
    fun a_link_answer_from_a_suspended_account_never_reaches_the_decision() = runTest {
        val suspendedViaLink = request(1, 1, link = true)
        val unknownViaLink = request(2, 2, link = true)
        val slowViaLink = request(3, 3, link = true)
        val blockedPlain = request(4, 4, link = false)
        val suspendedPlain = request(5, 5, link = false)
        val fine = request(6, 6, link = false)
        val asked = mutableListOf<String>()
        val kept = screenFetchedRequests(
            listOf(suspendedViaLink, unknownViaLink, slowViaLink, blockedPlain, suspendedPlain, fine),
            dropsBlocked = { it == b64(blockedPlain) },
            timeoutMs = 3_000,
        ) { key ->
            asked += key
            when (key) {
                b64(suspendedViaLink), b64(suspendedPlain) -> AccountStanding.SUSPENDED
                b64(slowViaLink) -> {
                    delay(10_000)
                    AccountStanding.SUSPENDED
                }
                b64(fine) -> AccountStanding.NOT_SUSPENDED
                else -> AccountStanding.UNKNOWN
            }
        }
        assertEquals(listOf(2L, 3L, 6L), kept.map { it.messageId })
        // The link answer survives on the requests that go on to be decided.
        assertNotNull(kept.first { it.messageId == 2L }.linkAnswer)
        assertNotNull(kept.first { it.messageId == 3L }.linkAnswer)
        // A blocked account is dropped without asking the log.
        assertEquals(false, b64(blockedPlain) in asked)
    }

    @Test
    fun a_held_request_leaves_the_list_once_its_sender_is_confirmed_suspended() {
        val a = request(1, 1, link = false)
        val b = request(2, 2, link = false)
        val c = request(3, 3, link = false)
        val pending = listOf(a, b, c)
        // Before any answer: everything shows.
        assertEquals(pending, shownPendingRequests(pending, emptySet()))
        // As answers arrive, in any base64 spelling.
        val urlB = Base64.getUrlEncoder().withoutPadding().encodeToString(b.fromPubkey)
        assertEquals(listOf(a, c), shownPendingRequests(pending, setOf(urlB)))
        assertEquals(listOf(c), shownPendingRequests(pending, setOf(urlB, b64(a))))
    }

    @Test
    fun an_undecided_link_request_is_dropped_at_decision_only_on_a_verified_suspension() = runTest {
        val req = request(7, 7, link = true)
        val asked = mutableListOf<String>()
        fun standing(s: AccountStanding): suspend (String) -> AccountStanding = { k -> asked += k; s }
        assertEquals(true, heldLinkRequestDropsOnSuspension(req, 3_000, standing(AccountStanding.SUSPENDED)))
        assertEquals(listOf(b64(req)), asked)
        assertEquals(false, heldLinkRequestDropsOnSuspension(req, 3_000, standing(AccountStanding.NOT_SUSPENDED)))
        assertEquals(false, heldLinkRequestDropsOnSuspension(req, 3_000, standing(AccountStanding.UNKNOWN)))
        val start = testScheduler.currentTime
        assertEquals(
            false,
            heldLinkRequestDropsOnSuspension(req, 3_000) {
                delay(10_000)
                AccountStanding.SUSPENDED
            },
        )
        assertEquals(3_000L, testScheduler.currentTime - start)
    }
}
