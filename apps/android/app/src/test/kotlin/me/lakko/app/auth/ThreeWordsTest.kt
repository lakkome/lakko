// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import me.lakko.app.l10n.UiText
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException

/**
 * The pure rules of "Three words from your phrase": whether the row is
 * listed, the local guess budget (five misses, then a 15-minute pause), its
 * persistence, and how a failure of the sign-in after a match reads.
 */
class ThreeWordsTest {

    private class MapStorage : MarkerStorage {
        val rows = HashMap<String, String>()
        override fun get(key: String): String? = rows[key]
        override fun set(key: String, value: String) {
            rows[key] = value
        }
        override fun remove(key: String) {
            rows.remove(key)
        }
    }

    // ---- row visibility --------------------------------------------------

    @Test
    fun `the row needs a cached username, the phrase and the keys`() {
        assertTrue(offersThreeWords("alice", phraseOnDevice = true, keysOnDevice = true, suspended = false))
        assertFalse(offersThreeWords(null, phraseOnDevice = true, keysOnDevice = true, suspended = false))
        // A guest account has no login handle to address the sign-in by.
        assertFalse(offersThreeWords("", phraseOnDevice = true, keysOnDevice = true, suspended = false))
        assertFalse(offersThreeWords("   ", phraseOnDevice = true, keysOnDevice = true, suspended = false))
        assertFalse(offersThreeWords("alice", phraseOnDevice = false, keysOnDevice = true, suspended = false))
        assertFalse(offersThreeWords("alice", phraseOnDevice = true, keysOnDevice = false, suspended = false))
    }

    // ---- budget ------------------------------------------------------------

    @Test
    fun `four misses leave tries, the fifth starts a fifteen minute pause`() {
        var budget = ThreeWordsBudget.FRESH
        val now = 1_000_000L
        repeat(4) { i ->
            budget = afterMiss(budget, now)
            assertNull(budget.pausedUntilMs)
            assertEquals(THREE_WORDS_MAX_MISSES - (i + 1), triesLeft(budget, now))
            assertEquals(0L, pauseRemainingMs(budget, now))
        }
        budget = afterMiss(budget, now)
        assertEquals(now + THREE_WORDS_PAUSE_MS, budget.pausedUntilMs)
        assertEquals(15L * 60L * 1000L, pauseRemainingMs(budget, now))
        assertEquals(0, triesLeft(budget, now))
    }

    @Test
    fun `a miss while paused changes nothing`() {
        val now = 5_000L
        val paused = ThreeWordsBudget(misses = 5, pausedUntilMs = now + THREE_WORDS_PAUSE_MS)
        assertEquals(paused, afterMiss(paused, now + 1_000L))
    }

    @Test
    fun `the count restarts at zero once the pause has run out`() {
        val now = 5_000L
        val paused = ThreeWordsBudget(misses = 5, pausedUntilMs = now + THREE_WORDS_PAUSE_MS)
        val after = now + THREE_WORDS_PAUSE_MS
        assertEquals(ThreeWordsBudget.FRESH, budgetAt(paused, after))
        assertEquals(0L, pauseRemainingMs(paused, after))
        assertEquals(THREE_WORDS_MAX_MISSES, triesLeft(paused, after))
        assertEquals(ThreeWordsBudget(misses = 1), afterMiss(paused, after))
    }

    @Test
    fun `a clock set back never yields more than a full pause`() {
        val pausedAt = 10_000_000L
        val paused = ThreeWordsBudget(misses = 5, pausedUntilMs = pausedAt + THREE_WORDS_PAUSE_MS)
        // The device clock was moved a day back.
        val earlier = pausedAt - 24L * 60L * 60L * 1000L
        assertEquals(THREE_WORDS_PAUSE_MS, pauseRemainingMs(paused, earlier))
    }

    @Test
    fun `the countdown reads minutes and seconds and never zero while it runs`() {
        assertEquals("15:00", formatCountdown(THREE_WORDS_PAUSE_MS))
        assertEquals("14:59", formatCountdown(THREE_WORDS_PAUSE_MS - 1_000L))
        assertEquals("0:01", formatCountdown(1L))
        assertEquals("0:09", formatCountdown(8_001L))
        assertEquals("0:00", formatCountdown(0L))
        assertEquals("0:00", formatCountdown(-5L))
    }

    // ---- persistence ---------------------------------------------------------

    @Test
    fun `the budget is stored per account and survives a new store`() {
        val storage = MapStorage()
        val paused = ThreeWordsBudget(misses = 5, pausedUntilMs = 42L)
        ThreeWordsBudgetStore(storage).save("acct-a", paused)
        ThreeWordsBudgetStore(storage).save("acct-b", ThreeWordsBudget(misses = 2))
        assertEquals(paused, ThreeWordsBudgetStore(storage).load("acct-a"))
        assertEquals(ThreeWordsBudget(misses = 2), ThreeWordsBudgetStore(storage).load("acct-b"))
        assertEquals(ThreeWordsBudget.FRESH, ThreeWordsBudgetStore(storage).load("acct-c"))
    }

    @Test
    fun `clearing one account leaves the other`() {
        val storage = MapStorage()
        val store = ThreeWordsBudgetStore(storage)
        store.save("acct-a", ThreeWordsBudget(misses = 3))
        store.save("acct-b", ThreeWordsBudget(misses = 1))
        store.clear("acct-a")
        assertEquals(ThreeWordsBudget.FRESH, store.load("acct-a"))
        assertEquals(ThreeWordsBudget(misses = 1), store.load("acct-b"))
    }

    @Test
    fun `a malformed row reads as fresh instead of locking the route`() {
        val storage = MapStorage()
        val store = ThreeWordsBudgetStore(storage)
        for (raw in listOf("", "not json", "[]", """{"misses":-3}""", """{"misses":"many"}""")) {
            storage.rows[ThreeWordsBudgetStore.key("acct")] = raw
            assertEquals("row: $raw", ThreeWordsBudget.FRESH, store.load("acct"))
        }
        storage.rows[ThreeWordsBudgetStore.key("acct")] = """{"misses":99,"pausedUntilMs":null}"""
        assertEquals(THREE_WORDS_MAX_MISSES, store.load("acct").misses)
    }

    @Test
    fun `a storage that throws neither loses the form nor crashes`() {
        val broken = object : MarkerStorage {
            override fun get(key: String): String? = throw IllegalStateException("blocked")
            override fun set(key: String, value: String) = throw IllegalStateException("blocked")
            override fun remove(key: String) = throw IllegalStateException("blocked")
        }
        val store = ThreeWordsBudgetStore(broken)
        store.save("acct", ThreeWordsBudget(misses = 2))
        store.clear("acct")
        assertEquals(ThreeWordsBudget.FRESH, store.load("acct"))
    }

    // ---- the form's button ------------------------------------------------------

    @Test
    fun `the button needs three words of the phrase word list`() {
        assertTrue(isPhraseWord("abandon"))
        assertTrue(isPhraseWord("  Zoo "))
        assertFalse(isPhraseWord(""))
        assertFalse(isPhraseWord("ab"))
        assertFalse("a well-shaped non-word is a typo, not a guess", isPhraseWord("abandom"))
        assertFalse(isPhraseWord("two words"))

        assertTrue(threeWordsSubmittable(listOf("abandon", "Zoo", " ability ")))
        assertFalse(threeWordsSubmittable(listOf("abandon", "zoo", "abandom")))
        assertFalse(threeWordsSubmittable(listOf("abandon", "zoo", "")))
        assertFalse(threeWordsSubmittable(listOf("abandon", "zoo")))
        assertFalse(threeWordsSubmittable(listOf("abandon", "zoo", "ability", "able")))
    }

    // ---- a refused sign-in suspends the route ---------------------------------------

    @Test
    fun `only a refused credential counts as invalid credentials`() {
        assertEquals(
            RecoveryLoginFailureKind.InvalidCredentials,
            recoveryLoginFailureKind(AuthHttpError(401, "/recovery/login/finish", "")),
        )
        for (status in listOf(400, 404, 429, 500, 503)) {
            assertEquals(
                "status $status",
                RecoveryLoginFailureKind.Other,
                recoveryLoginFailureKind(AuthHttpError(status, "/recovery/login/start", "")),
            )
        }
        assertEquals(
            RecoveryLoginFailureKind.Other,
            recoveryLoginFailureKind(AuthNetworkError("/recovery/login/start", java.io.IOException("offline"))),
        )
        assertEquals(RecoveryLoginFailureKind.Other, recoveryLoginFailureKind(IllegalStateException("x")))
        assertEquals(
            RecoveryLoginFailureKind.Other,
            recoveryLoginFailureKind(AuthClientArgError("another account")),
        )
    }

    @Test
    fun `a refused credential suspends three words`() {
        // ADR-0065: a reset replaces the recovery credential in the same
        // server transaction that stores the new seed here, so a refusal is
        // never "the new phrase is not registered yet".
        assertEquals(
            StoredPhraseVerdict.SuspendThreeWords,
            storedPhraseVerdict(RecoveryLoginFailureKind.InvalidCredentials),
        )
    }

    @Test
    fun `a network error or a limit never suspends the route`() {
        assertEquals(StoredPhraseVerdict.Keep, storedPhraseVerdict(RecoveryLoginFailureKind.Other))
        // End to end over the errors the sign-in throws.
        val harmless = listOf<Throwable>(
            AuthNetworkError("/recovery/login/start", java.io.IOException("offline")),
            AuthHttpError(429, "/recovery/login/start", "", code = "too-many-attempts", retryAfterSecs = 30),
            AuthHttpError(503, "/recovery/login/start", "", code = "rate-limiter-unavailable"),
            AuthHttpError(503, "/recovery/login/start", "", code = "login-by-name-disabled"),
        )
        for (err in harmless) {
            assertEquals(
                err.toString(),
                StoredPhraseVerdict.Keep,
                storedPhraseVerdict(recoveryLoginFailureKind(err)),
            )
        }
    }

    @Test
    fun `the form reads a suspension as its own failure`() {
        val refused = AuthHttpError(401, "/recovery/login/finish", "")
        assertEquals(ThreeWordsFailure.Suspended, threeWordsFailureOf(ThreeWordsSuspendedError(refused)))
        assertEquals(ThreeWordsFailure.Rejected, threeWordsFailureOf(refused))
    }

    @Test
    fun `the settings row says not here, paused, or available, in that order`() {
        assertEquals(
            "settings.recovery.words.not_on_this_device",
            threeWordsSettingsLine(phraseSaved = false, suspended = false),
        )
        assertEquals(
            "settings.recovery.words.not_on_this_device",
            threeWordsSettingsLine(phraseSaved = false, suspended = true),
        )
        assertEquals(
            "settings.recovery.words.suspended",
            threeWordsSettingsLine(phraseSaved = true, suspended = true),
        )
        assertEquals(
            "settings.recovery.words.on_this_device",
            threeWordsSettingsLine(phraseSaved = true, suspended = false),
        )
    }

    @Test
    fun `a suspended route is not offered`() {
        assertTrue(offersThreeWords("alice", phraseOnDevice = true, keysOnDevice = true, suspended = false))
        assertFalse(offersThreeWords("alice", phraseOnDevice = true, keysOnDevice = true, suspended = true))
    }

    @Test
    fun `suspending is per account, survives a restart, clears the budget and is lifted`() {
        val storage = MapStorage()
        val store = ThreeWordsBudgetStore(storage)
        store.save("acct-a", ThreeWordsBudget(misses = 3))
        store.save("acct-b", ThreeWordsBudget(misses = 2))
        assertFalse(store.isSuspended("acct-a"))

        store.suspend("acct-a")
        assertTrue(ThreeWordsBudgetStore(storage).isSuspended("acct-a"))
        assertFalse(store.isSuspended("acct-b"))
        assertEquals(ThreeWordsBudget.FRESH, store.load("acct-a"))
        assertEquals(ThreeWordsBudget(misses = 2), store.load("acct-b"))

        // A landing path wrote the phrase store again.
        store.clearSuspended("acct-a")
        assertFalse(store.isSuspended("acct-a"))

        // Forget device / account deletion: both rows go.
        store.suspend("acct-b")
        store.save("acct-b", ThreeWordsBudget(misses = 1))
        store.clearAccount("acct-b")
        assertFalse(store.isSuspended("acct-b"))
        assertEquals(ThreeWordsBudget.FRESH, store.load("acct-b"))
        assertTrue(storage.rows.isEmpty())
    }

    // ---- failures after a match ----------------------------------------------------

    @Test
    fun `a server limit with a wait is a countdown`() {
        val err = AuthHttpError(429, "/recovery/login/start", "", code = "too-many-attempts", retryAfterSecs = 90)
        assertEquals(90L, attemptsRetryAfterSecs(err))
        assertEquals(ThreeWordsFailure.RateLimited(90), threeWordsFailureOf(err))
        // Another limit's code has its own copy and is not counted down here.
        val other = AuthHttpError(429, "/x", "", code = "device-request-limited", retryAfterSecs = 90)
        assertNull(attemptsRetryAfterSecs(other))
        assertNull(attemptsRetryAfterSecs(AuthHttpError(401, "/x", "")))
    }

    @Test
    fun `a stale phrase is rejected, an outage is unavailable, a dead network is offline`() {
        assertEquals(ThreeWordsFailure.Rejected, threeWordsFailureOf(AuthHttpError(401, "/recovery/login/finish", "")))
        assertEquals(ThreeWordsFailure.Rejected, threeWordsFailureOf(AuthHttpError(404, "/recovery/login/start", "")))
        assertEquals(ThreeWordsFailure.Unavailable, threeWordsFailureOf(AuthHttpError(503, "/x", "")))
        assertEquals(
            ThreeWordsFailure.Generic,
            threeWordsFailureOf(AuthHttpError(503, "/x", "", code = "rate-limiter-unavailable")),
        )
        assertEquals(ThreeWordsFailure.Offline, threeWordsFailureOf(AuthNetworkError("/x", IOException("down"))))
        assertEquals(ThreeWordsFailure.Generic, threeWordsFailureOf(IllegalStateException("boom")))
    }

    @Test
    fun `a refusal that carries its own sentence shows it`() {
        val text = UiText("recovery.words.error.rejected")
        assertEquals(ThreeWordsFailure.Sentence(text), threeWordsFailureOf(AuthClientArgError("other account", text)))
        assertEquals(ThreeWordsFailure.Generic, threeWordsFailureOf(AuthClientArgError("contract")))
        // A limit without a stated wait keeps the limit sentence.
        val limit = threeWordsFailureOf(AuthHttpError(429, "/x", "", code = "too-many-attempts"))
        assertTrue(limit is ThreeWordsFailure.Sentence)
    }
}
