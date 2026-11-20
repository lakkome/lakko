// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.account

import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.test.runTest
import me.lakko.app.anon.AnonSession
import me.lakko.app.anon.AnonTokenContext
import me.lakko.app.anon.AnonTokenSource
import me.lakko.app.anon.PooledAnonToken
import me.lakko.app.anon.PrefetchedAnonTokens
import me.lakko.app.push.PushSubscriptionsLeft
import me.lakko.app.push.PushSubscriptionsRemoval
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Test

/** The order of Delete account: auth, then the server cleanups, then the wipe. */
class AccountDeletionTest {

    private val events = mutableListOf<String>()
    private val logs = mutableListOf<String>()

    private fun deletion(
        prepare: suspend () -> Unit = {},
        deleteAtAuth: suspend () -> Unit = { events += "auth" },
        cleanups: List<AccountDeletion.Cleanup>,
        attempts: Int = 3,
        perCleanupBoundMs: Long = 8_000,
        overallBoundMs: Long = 10_000,
        prepareBoundMs: Long = 6_000,
    ) = AccountDeletion(
        prepare = prepare,
        deleteAtAuth = deleteAtAuth,
        cleanups = cleanups,
        wipe = { events += "wipe" },
        attempts = attempts,
        retryDelayMs = 100,
        perCleanupBoundMs = perCleanupBoundMs,
        overallBoundMs = overallBoundMs,
        prepareBoundMs = prepareBoundMs,
        log = { logs += it },
    )

    private fun recording(name: String) = AccountDeletion.Cleanup(name) { events += name }

    @Test
    fun auth_first_then_every_cleanup_then_the_wipe() = runTest {
        deletion(cleanups = listOf(recording("push"), recording("backup"), recording("directory"))).run()

        assertEquals("auth", events.first())
        assertEquals("wipe", events.last())
        assertEquals(setOf("push", "backup", "directory"), events.subList(1, 4).toSet())
        assertEquals(5, events.size)
        assertTrue(logs.isEmpty())
    }

    @Test
    fun a_refused_auth_delete_runs_no_cleanup_and_no_wipe() = runTest {
        val refused = IllegalStateException("recall-window-open")
        try {
            deletion(
                deleteAtAuth = { throw refused },
                cleanups = listOf(recording("push"), recording("backup")),
            ).run()
            fail("the refusal must reach the caller")
        } catch (e: IllegalStateException) {
            assertEquals(refused, e)
        }
        assertTrue(events.isEmpty())
    }

    @Test
    fun a_failing_cleanup_is_retried_and_does_not_stop_the_others_or_the_wipe() = runTest {
        var tries = 0
        val failing = AccountDeletion.Cleanup("backup") {
            tries += 1
            throw java.io.IOException("offline")
        }
        deletion(cleanups = listOf(failing, recording("directory"))).run()

        assertEquals(3, tries)
        assertEquals(listOf("auth", "directory", "wipe"), events)
        assertEquals(listOf("account deletion: backup failed: IOException"), logs)
    }

    @Test
    fun a_cleanup_that_succeeds_on_a_retry_logs_nothing() = runTest {
        var tries = 0
        val flaky = AccountDeletion.Cleanup("push") {
            tries += 1
            if (tries < 2) throw java.io.IOException("offline")
            events += "push"
        }
        deletion(cleanups = listOf(flaky)).run()

        assertEquals(listOf("auth", "push", "wipe"), events)
        assertTrue(logs.isEmpty())
    }

    @Test
    fun a_hanging_cleanup_is_cut_off_and_the_wipe_still_runs() = runTest {
        val hanging = AccountDeletion.Cleanup("push") { awaitCancellation() }
        deletion(
            cleanups = listOf(hanging, recording("directory")),
            perCleanupBoundMs = 1_000,
            overallBoundMs = 5_000,
        ).run()

        assertEquals(listOf("auth", "directory", "wipe"), events)
        assertEquals(listOf("account deletion: push ran out of time"), logs)
    }

    @Test
    fun the_overall_bound_cuts_off_every_cleanup() = runTest {
        val hanging = AccountDeletion.Cleanup("push") { awaitCancellation() }
        deletion(cleanups = listOf(hanging), perCleanupBoundMs = 60_000, overallBoundMs = 2_000).run()

        assertEquals(listOf("auth", "wipe"), events)
        assertEquals(listOf("account deletion: cleanups ran out of time"), logs)
    }

    @Test
    fun a_stray_cancellation_from_a_cleanup_is_a_failure_not_a_skipped_wipe() = runTest {
        val stray = AccountDeletion.Cleanup("backup") {
            throw kotlinx.coroutines.CancellationException("library cancelled")
        }
        deletion(cleanups = listOf(stray), attempts = 1).run()

        assertEquals(listOf("auth", "wipe"), events)
        assertEquals(listOf("account deletion: backup failed: CancellationException"), logs)
    }

    @Test
    fun push_removal_retries_only_the_ids_left() = runTest {
        val a = ByteArray(16) { 1 }
        val b = ByteArray(16) { 2 }
        val removal = PushSubscriptionsRemoval(listOf(a, b, a.copyOf()))
        val sent = mutableListOf<Byte>()
        var bFails = true
        val unsubscribe: suspend (ByteArray) -> Unit = { id ->
            sent += id[0]
            if (id[0] == 2.toByte() && bFails) throw java.io.IOException("offline")
        }

        try {
            removal.run(unsubscribe)
            fail("one id is left")
        } catch (e: PushSubscriptionsLeft) {
            assertEquals(1, e.count)
        }
        assertEquals(1, removal.left)
        bFails = false
        removal.run(unsubscribe)

        assertEquals(listOf<Byte>(1, 2, 2), sent)
        assertEquals(0, removal.left)
    }

    @Test
    fun push_tokens_are_reserved_before_the_auth_delete_one_per_subscription_plus_spare() = runTest {
        val subscriptions = 5
        val asked = mutableListOf<Int>()
        deletion(
            prepare = {
                asked += AccountDeletion.pushTokensNeeded(subscriptions)
                events += "reserve"
            },
            cleanups = listOf(recording("push")),
        ).run()

        assertEquals(listOf("reserve", "auth", "push", "wipe"), events)
        assertEquals(listOf(subscriptions + AccountDeletion.PUSH_TOKEN_SPARE), asked)
        assertEquals(0, AccountDeletion.pushTokensNeeded(0))
    }

    @Test
    fun a_failed_or_slow_reserve_does_not_stop_the_deletion() = runTest {
        deletion(prepare = { throw java.io.IOException("offline") }, cleanups = emptyList()).run()
        deletion(prepare = { awaitCancellation() }, cleanups = emptyList(), prepareBoundMs = 1_000).run()

        assertEquals(listOf("auth", "wipe", "auth", "wipe"), events)
        assertEquals(
            listOf("account deletion: preparing failed: IOException", "account deletion: preparing ran out of time"),
            logs,
        )
    }

    @Test
    fun reserved_tokens_are_spent_before_the_pool() = runTest {
        val pool = object : AnonTokenSource {
            var taken = 0
            override val headerName: String = "x-lakko-anon-token"
            override suspend fun take(context: AnonTokenContext): PooledAnonToken {
                taken += 1
                return PooledAnonToken(context, 1, "pool-$taken")
            }
            override suspend fun takeDetached(session: AnonSession, context: AnonTokenContext) = take(context)
            override fun releaseSession() = Unit
        }
        val source = PrefetchedAnonTokens(emptyList(), pool)
        source.add(
            listOf(
                PooledAnonToken(AnonTokenContext.PUSH_REGISTER, 1, "reserved-1"),
                PooledAnonToken(AnonTokenContext.PUSH_REGISTER, 1, "reserved-2"),
            ),
        )

        val spent = (1..3).map { source.take(AnonTokenContext.PUSH_REGISTER).headerValue }
        assertEquals(listOf("reserved-1", "reserved-2", "pool-1"), spent)
        assertEquals(1, pool.taken)
    }
}
