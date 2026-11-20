// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import java.util.Base64
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0089 step 4 — the suspension standings service and the decisions each
 * mark site takes from it. The host suite has no native core, so the bucket
 * prefix and the log check are fakes; the service's own rules (one request in
 * flight per prefix, reuse until the snapshot's reuse time, a ten-minute back
 * off after a failure, UNKNOWN on any check error, nothing asked unpinned) are
 * what is tested here.
 */
class SuspensionMarksTest {

    private val pins = ModerationLogPins(ByteArray(32) { 7 }, emptyList(), 1)

    private fun key(first: Int, second: Int, fill: Int = 0): ByteArray =
        ByteArray(32) { fill.toByte() }.also {
            it[0] = first.toByte()
            it[1] = second.toByte()
        }

    private fun b64(bytes: ByteArray): String = Base64.getEncoder().encodeToString(bytes)

    private fun prefix(bytes: ByteArray): String = "%02x%02x".format(bytes[0].toInt() and 0xff, bytes[1].toInt() and 0xff)

    private class Fakes {
        var now = 1_000_000L
        val fetched = mutableListOf<String>()
        var answer: String? = "answer"
        var reuseUntil = 0L
        var suspendedKeys = emptySet<String>()
        var verifyThrows = false
        var gate: CompletableDeferred<Unit>? = null
    }

    private fun service(f: Fakes, scope: CoroutineScope, withPins: Boolean = true) = SuspensionStandings(
        pins = if (withPins) pins else null,
        prefixOf = ::prefix,
        fetch = { p ->
            f.fetched += p
            f.gate?.await()
            f.answer
        },
        verify = { _, key, _, _ ->
            if (f.verifyThrows) throw IllegalStateException("stale: snapshot too old")
            val suspended = b64(key) in f.suspendedKeys
            VerifiedStanding(suspended = suspended, reuseUntilMs = f.reuseUntil, logIndex = if (suspended) 5uL else null)
        },
        scope = scope,
        nowMs = { f.now },
        // ADR-0089 F2: these tests are about the service's own rules; every
        // suspension here is proven (the proof rules have their own tests).
        prove = { _, _, _, _ -> true },
    )

    @After
    fun reset() {
        SuspensionMarks.overrideForTests(null)
        SuspensionMarks.ownAccountKey = { null }
    }

    @Test
    fun unpinned_asks_nothing_and_answers_unknown() = runTest {
        val f = Fakes()
        val s = service(f, this, withPins = false)
        assertEquals(AccountStanding.UNKNOWN, s.standing(b64(key(1, 2))))
        assertTrue(f.fetched.isEmpty())
        assertFalse(s.enabled)
    }

    @Test
    fun pinned_log_key_is_read_only_when_well_formed() {
        assertNull(pinnedModerationLogKey(""))
        assertNull(pinnedModerationLogKey("not base64 at all!"))
        assertNull(pinnedModerationLogKey(Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(31))))
        val good = ByteArray(32) { it.toByte() }
        val read = pinnedModerationLogKey(Base64.getUrlEncoder().withoutPadding().encodeToString(good))
        assertTrue(read!!.contentEquals(good))
    }

    @Test
    fun a_verified_answer_is_reused_until_its_reuse_time() = runTest {
        val f = Fakes().apply { reuseUntil = now + 60_000 }
        val s = service(f, this)
        val k = key(1, 2)
        f.suspendedKeys = setOf(b64(k))
        assertEquals(AccountStanding.SUSPENDED, s.standing(b64(k)))
        assertEquals(AccountStanding.SUSPENDED, s.standing(b64(k)))
        // Another account in the same bucket reads the same answer.
        assertEquals(AccountStanding.NOT_SUSPENDED, s.standing(b64(key(1, 2, fill = 9))))
        assertEquals(listOf("0102"), f.fetched)
        f.now += 60_000
        f.reuseUntil = f.now + 60_000
        assertEquals(AccountStanding.SUSPENDED, s.standing(b64(k)))
        assertEquals(listOf("0102", "0102"), f.fetched)
    }

    @Test
    fun one_request_in_flight_per_prefix() = runTest {
        val f = Fakes().apply {
            reuseUntil = now + 60_000
            gate = CompletableDeferred()
        }
        val s = service(f, this)
        val a = key(3, 4, fill = 1)
        val b = key(3, 4, fill = 2)
        val other = key(5, 6)
        f.suspendedKeys = setOf(b64(b))
        val first = async { s.standing(b64(a)) }
        val second = async { s.standing(b64(b)) }
        val third = async { s.standing(b64(other)) }
        advanceUntilIdle()
        assertEquals(listOf("0304", "0506"), f.fetched.sorted())
        f.gate!!.complete(Unit)
        assertEquals(AccountStanding.NOT_SUSPENDED, first.await())
        assertEquals(AccountStanding.SUSPENDED, second.await())
        assertEquals(AccountStanding.NOT_SUSPENDED, third.await())
        assertEquals(2, f.fetched.size)
    }

    @Test
    fun a_failed_fetch_is_not_retried_for_ten_minutes() = runTest {
        val f = Fakes().apply {
            answer = null
            reuseUntil = now + 60_000
        }
        val s = service(f, this)
        val k = b64(key(7, 8))
        assertEquals(AccountStanding.UNKNOWN, s.standing(k))
        f.answer = "answer"
        f.now += SuspensionStandings.FAILURE_BACKOFF_MS - 1
        assertEquals(AccountStanding.UNKNOWN, s.standing(k))
        assertEquals(1, f.fetched.size)
        f.now += 1
        f.reuseUntil = f.now + 60_000
        assertEquals(AccountStanding.NOT_SUSPENDED, s.standing(k))
        assertEquals(2, f.fetched.size)
    }

    @Test
    fun any_check_error_is_unknown_and_backs_off() = runTest {
        val f = Fakes().apply {
            verifyThrows = true
            reuseUntil = now + 60_000
        }
        val s = service(f, this)
        val k = key(9, 9)
        f.suspendedKeys = setOf(b64(k))
        assertEquals(AccountStanding.UNKNOWN, s.standing(b64(k)))
        f.verifyThrows = false
        assertEquals(AccountStanding.UNKNOWN, s.standing(b64(k)))
        assertEquals(1, f.fetched.size)
    }

    @Test
    fun a_kept_answer_that_no_longer_verifies_is_dropped() = runTest {
        val f = Fakes().apply { reuseUntil = now + 60_000 }
        val s = service(f, this)
        val k = key(2, 2)
        f.suspendedKeys = setOf(b64(k))
        assertEquals(AccountStanding.SUSPENDED, s.standing(b64(k)))
        f.verifyThrows = true
        assertEquals(AccountStanding.UNKNOWN, s.standing(b64(k)))
        f.verifyThrows = false
        f.now += SuspensionStandings.FAILURE_BACKOFF_MS
        assertEquals(AccountStanding.SUSPENDED, s.standing(b64(k)))
        assertEquals(2, f.fetched.size)
    }

    @Test
    fun a_malformed_key_or_prefix_asks_nothing() = runTest {
        val f = Fakes()
        val s = SuspensionStandings(
            pins = pins,
            prefixOf = { "zz" },
            fetch = { f.fetched += it; "answer" },
            verify = { _, _, _, _ -> VerifiedStanding(true, Long.MAX_VALUE, 1uL) },
            scope = this,
            prove = { _, _, _, _ -> true },
        )
        assertEquals(AccountStanding.UNKNOWN, s.standing(b64(key(1, 1))))
        assertEquals(AccountStanding.UNKNOWN, s.standing("short"))
        assertEquals(AccountStanding.UNKNOWN, s.standing(null))
        assertTrue(f.fetched.isEmpty())
    }

    @Test
    fun this_device_s_own_account_is_never_marked() = runTest {
        val f = Fakes().apply { reuseUntil = now + 60_000 }
        val own = key(4, 4)
        f.suspendedKeys = setOf(b64(own))
        SuspensionMarks.overrideForTests(service(f, this))
        SuspensionMarks.ownAccountKey = { b64(own) }
        assertEquals(AccountStanding.UNKNOWN, SuspensionMarks.standing(b64(own)))
        // Any base64 spelling of the own key.
        assertFalse(SuspensionMarks.isSuspended(Base64.getUrlEncoder().withoutPadding().encodeToString(own)))
        assertTrue(f.fetched.isEmpty())
        SuspensionMarks.ownAccountKey = { b64(key(8, 8)) }
        assertTrue(SuspensionMarks.isSuspended(b64(own)))
    }

    @Test
    fun the_block_mark_wins_over_the_suspended_mark() {
        assertTrue(suspendedBannerShows(blocked = false, suspended = true))
        assertFalse(suspendedBannerShows(blocked = true, suspended = true))
        assertFalse(suspendedBannerShows(blocked = false, suspended = false))
        assertTrue(suspendedCardLineShows(blocked = false, suspended = true))
        assertFalse(suspendedCardLineShows(blocked = true, suspended = true))
        assertFalse(suspendedCardLineShows(blocked = true, suspended = false))
    }

    @Test
    fun a_request_is_dropped_only_on_a_verified_suspension() = runTest {
        val suspended = key(1, 0)
        val notSuspended = key(2, 0)
        val unknown = key(3, 0)
        val slow = key(4, 0)
        val requests = listOf(suspended, notSuspended, unknown, slow)
        val kept = filterSuspendedRequests(requests, keyOf = { it }, timeoutMs = 3_000) { k ->
            when (k) {
                b64(suspended) -> AccountStanding.SUSPENDED
                b64(notSuspended) -> AccountStanding.NOT_SUSPENDED
                b64(slow) -> {
                    delay(10_000)
                    AccountStanding.SUSPENDED
                }
                else -> AccountStanding.UNKNOWN
            }
        }
        assertEquals(listOf(notSuspended, unknown, slow), kept)
        assertFalse(dropsSuspendedRequest(null))
        assertFalse(dropsSuspendedRequest(AccountStanding.UNKNOWN))
        assertTrue(dropsSuspendedRequest(AccountStanding.SUSPENDED))
    }

    @Test
    fun invite_add_stops_only_on_a_verified_suspension() = runTest {
        val inviter = key(5, 5)
        val asked = mutableListOf<String>()
        fun standingIs(s: AccountStanding): suspend (String) -> AccountStanding = { k -> asked += k; s }
        assertFalse(inviteAddMayProceed(inviter, 3_000, standingIs(AccountStanding.SUSPENDED)))
        assertEquals(listOf(b64(inviter)), asked)
        assertTrue(inviteAddMayProceed(inviter, 3_000, standingIs(AccountStanding.NOT_SUSPENDED)))
        assertTrue(inviteAddMayProceed(inviter, 3_000, standingIs(AccountStanding.UNKNOWN)))
        // A check slower than the bound lets Add proceed, after the bound.
        val start = testScheduler.currentTime
        assertTrue(
            inviteAddMayProceed(inviter, 3_000) {
                delay(10_000)
                AccountStanding.SUSPENDED
            },
        )
        assertEquals(3_000, testScheduler.currentTime - start)
    }

    @Test
    fun invite_add_waits_for_an_answer_still_in_flight() = runTest {
        val f = Fakes().apply {
            reuseUntil = now + 60_000
            gate = CompletableDeferred()
        }
        val inviter = key(7, 7)
        f.suspendedKeys = setOf(b64(inviter))
        SuspensionMarks.overrideForTests(service(f, this))
        // The page's own check is already running when Add is tapped.
        val pageCheck = async { SuspensionMarks.standing(b64(inviter)) }
        val add = async { inviteAddMayProceed(inviter) }
        // Within the bound: the answer lands after one virtual second.
        runCurrent()
        assertEquals(1, f.fetched.size)
        advanceTimeBy(1_000)
        f.gate!!.complete(Unit)
        assertFalse(add.await())
        assertEquals(AccountStanding.SUSPENDED, pageCheck.await())
        assertEquals(1, f.fetched.size)
    }

    @Test
    fun clear_forgets_every_kept_standing_and_the_own_key() = runTest {
        val f = Fakes().apply { reuseUntil = now + 60_000 }
        val s = service(f, this)
        val k = key(1, 9)
        f.suspendedKeys = setOf(b64(k))
        assertEquals(AccountStanding.SUSPENDED, s.standing(b64(k)))
        // A failed prefix is forgotten too.
        f.answer = null
        assertEquals(AccountStanding.UNKNOWN, s.standing(b64(key(2, 9))))
        f.answer = "answer"
        s.clear()
        assertEquals(AccountStanding.SUSPENDED, s.standing(b64(k)))
        assertEquals(AccountStanding.NOT_SUSPENDED, s.standing(b64(key(2, 9))))
        assertEquals(listOf("0109", "0209", "0109", "0209"), f.fetched)

        SuspensionMarks.overrideForTests(s)
        SuspensionMarks.bindOwnAccountKey { b64(k) }
        assertFalse(SuspensionMarks.isSuspended(b64(k)))
        SuspensionMarks.clear()
        // The next account starts with no binding and no kept answer.
        assertTrue(SuspensionMarks.isSuspended(b64(k)))
        assertEquals(5, f.fetched.size)
    }

    @Test
    fun an_answer_landing_after_clear_is_not_kept() = runTest {
        val f = Fakes().apply {
            reuseUntil = now + 60_000
            gate = CompletableDeferred()
        }
        val s = service(f, this)
        val k = key(3, 9)
        f.suspendedKeys = setOf(b64(k))
        val before = async { s.standing(b64(k)) }
        runCurrent()
        s.clear()
        f.gate!!.complete(Unit)
        assertEquals(AccountStanding.UNKNOWN, before.await())
        f.gate = null
        assertEquals(AccountStanding.SUSPENDED, s.standing(b64(k)))
        assertEquals(2, f.fetched.size)
    }

    @Test
    fun the_request_poll_drops_through_the_marks_service() = runTest {
        val f = Fakes().apply { reuseUntil = now + 60_000 }
        val bad = key(6, 1)
        val good = key(6, 2)
        f.suspendedKeys = setOf(b64(bad))
        SuspensionMarks.overrideForTests(service(f, this))
        val kept = SuspensionMarks.withoutSuspendedRequests(listOf(bad, good), keyOf = { it })
        assertEquals(listOf(good), kept)
    }

    @Test
    fun search_leaves_out_accounts_confirmed_suspended() {
        val a = key(1, 1)
        val b = key(2, 2)
        val rows = listOf(b64(a), b64(b))
        assertEquals(rows, withoutSuspendedRows(rows, emptySet()) { it })
        // The suspended set may name the key in another base64 spelling.
        val urlA = Base64.getUrlEncoder().withoutPadding().encodeToString(a)
        assertEquals(listOf(b64(b)), withoutSuspendedRows(rows, setOf(urlA)) { it })
    }
}
