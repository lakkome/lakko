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
        first: List<AccountDeletion.Cleanup> = emptyList(),
        cleanups: List<AccountDeletion.Cleanup>,
        attempts: Int = 3,
        perCleanupBoundMs: Long = 8_000,
        overallBoundMs: Long = 10_000,
        prepareBoundMs: Long = 6_000,
    ) = AccountDeletion(
        prepare = prepare,
        deleteAtAuth = deleteAtAuth,
        first = first,
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

    /** ADR-0098, walk 10 - the statement runs to the end before the
     *  directory entry is removed: both at once rebuilt the conversation
     *  identity and stopped the statement in every conversation. */
    @Test
    fun the_first_cleanup_ends_before_any_other_cleanup_starts() = runTest {
        val statement = AccountDeletion.Cleanup("conversations") {
            events += "statement start"
            kotlinx.coroutines.delay(1_000)
            events += "statement end"
        }
        deletion(first = listOf(statement), cleanups = listOf(recording("directory"), recording("push"))).run()

        assertEquals(listOf("auth", "statement start", "statement end"), events.subList(0, 3))
        assertEquals(setOf("directory", "push"), events.subList(3, 5).toSet())
        assertEquals("wipe", events.last())
    }

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

    // ---- ADR-0098: the account-deleted statement -------------------------

    @Test
    fun the_statement_is_sent_after_the_auth_delete_and_before_the_wipe() = runTest {
        val announcement = accountDeletedAnnouncement(seeds = listOf(ByteArray(64))) { _, _ -> events += "conversations" }
        deletion(cleanups = listOf(announcement)).run()
        assertEquals(listOf("auth", "conversations", "wipe"), events)
    }

    @Test
    fun the_statement_is_never_sent_when_auth_refuses() = runTest {
        val announcement = accountDeletedAnnouncement(seeds = listOf(ByteArray(64))) { _, _ -> events += "conversations" }
        try {
            deletion(
                deleteAtAuth = { throw java.io.IOException("refused") },
                cleanups = listOf(announcement),
            ).run()
            fail("the auth refusal must propagate")
        } catch (_: java.io.IOException) {
            // expected
        }
        assertTrue(events.isEmpty())
    }

    @Test
    fun a_retry_reaches_only_the_conversations_not_yet_reached_and_a_failure_still_wipes() = runTest {
        val all = listOf("g1", "g2", "g3")
        val reached = mutableListOf<String>()
        var attempt = 0
        val announcement = accountDeletedAnnouncement(seeds = listOf(ByteArray(64))) { _, sent ->
            attempt += 1
            for (g in all) {
                if (g in sent) continue
                // g2 fails on the first attempt only.
                if (g == "g2" && attempt == 1) continue
                reached += g
                sent += g
            }
            if (sent.size < all.size) throw IllegalStateException("not all reached")
        }
        deletion(cleanups = listOf(announcement)).run()
        assertEquals(listOf("g1", "g3", "g2"), reached)
        assertEquals("wipe", events.last())

        events.clear()
        logs.clear()
        val failing = accountDeletedAnnouncement(seeds = emptyList()) { _, _ -> throw IllegalStateException("x") }
        deletion(cleanups = listOf(failing), attempts = 2).run()
        assertEquals(listOf("auth", "wipe"), events)
        assertEquals(listOf("account deletion: conversations failed: IllegalStateException"), logs)
    }
    /**
     * Walk 8 (ADR-0098): after DELETE /account a cleanup's bearer request
     * met 401, the refresh answered refresh-invalid, the device signed out
     * (the account store sealed), and the statement could not be posted.
     * With the provider told after the auth delete, nothing signs out and
     * every conversation gets its statement.
     */
    @Test
    fun a_refused_session_after_the_auth_delete_signs_nothing_out_and_every_conversation_is_told() = runTest {
        val refresher = object : me.lakko.app.auth.SessionTokenProvider.SessionRefresher {
            var calls = 0
            override suspend fun refresh(refreshToken: String): me.lakko.app.auth.SessionPair {
                calls += 1
                throw me.lakko.app.auth.AuthHttpError(
                    401, "/auth/session/refresh", """{"error":"refresh-invalid"}""", code = "refresh-invalid",
                )
            }
        }
        val acct = "AAAAAAAAAAAAAAAAAAAAAA"
        val dev = "D".repeat(43)
        val now = 1_000_000L
        val provider = me.lakko.app.auth.SessionTokenProvider(refresher, clock = { now })
        val token = "v3.$acct.1.$dev.1.${now + 60_000L}.access1"
        val refusals = mutableListOf<me.lakko.app.auth.SessionRefusal>()
        provider.install(
            me.lakko.app.auth.SessionPair(
                accessToken = token,
                accessExpiresAtMs = now + 60_000L,
                refreshToken = "r2.$acct.1.$dev.1.9.refresh1",
                refreshExpiresAtMs = now + 1_000_000L,
                keyGeneration = 1L,
            ),
            persist = {},
            onRefusal = { refusals += it },
        )
        val conversations = listOf("one-to-one", "group-a", "group-b")
        val posted = mutableSetOf<String>()
        val announce = accountDeletedAnnouncement(emptyList()) { _, sent ->
            var failures = 0
            for (c in conversations) {
                if (c in sent) continue
                // logout() seals the account store: the persist under the
                // statement's send then throws.
                if (refusals.isNotEmpty()) failures++ else { posted += c; sent += c }
            }
            if (failures > 0) throw IllegalStateException("$failures conversations not reached")
        }
        AccountDeletion(
            deleteAtAuth = { events += "auth" },
            afterAuth = { provider.markAccountDeleted() },
            cleanups = listOf(
                // A bearer request after the delete: near expiry, then a 401.
                AccountDeletion.Cleanup("push subscriptions") {
                    provider.current()
                    provider.afterUnauthorized(token)
                },
                announce,
            ),
            wipe = { events += "wipe" },
            retryDelayMs = 10,
            log = { logs += it },
        ).run()

        assertTrue(refusals.isEmpty())
        assertEquals(0, refresher.calls)
        assertEquals(conversations.toSet(), posted)
        assertEquals(listOf("auth", "wipe"), events)
        assertTrue(logs.isEmpty())
    }
}
