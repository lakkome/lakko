// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/*
 * "Three words from your phrase": the pure rules of the local three-word
 * check. Mirror of `apps/web/src/auth/recover/three-words.ts`.
 *
 * The check is LOCAL, like a wallet's "enter words 7, 3 and 12". A device
 * that holds the account's keys also holds the recovery phrase. Forgot the
 * password on such a device: the client draws three fresh random positions,
 * the person types those words, the client compares them with the phrase it
 * holds, and on a match signs in with the seed it already has. No server
 * takes part in the check and none stores anything about the words.
 *
 * Two rules live here, both pure so the host tests can pin them:
 *
 *   - [offersThreeWords]: whether the row is on the recovery list at all;
 *   - the guess budget: five misses, then a 15-minute pause.
 *
 * What the budget is, stated plainly: a CLIENT-SIDE gate in front of a
 * credential this device already holds. The phrase sits in this app's
 * Keystore-backed storage, which the app can open without the password.
 * Someone who can run their own code as this app does not need to guess
 * three words. The budget protects against exactly one thing: a person at
 * the unlocked device typing guesses into this form. It is not a server
 * limit and must never be described as one.
 */

/** How many words the person answers with. */
public const val THREE_WORDS: Int = 3

/**
 * Whether the recovery list offers "Three words from your phrase" on this
 * device. Every input is a local fact, so the answer says nothing about any
 * account on the server:
 *
 *   - an account is cached on this device and has a username (the sign-in
 *     that follows the check is addressed by the name's login handle; a
 *     guest account has none);
 *   - the device holds the recovery phrase for that account;
 *   - the device holds the account's keys (else the sign-in would only
 *     reach "Restore your keys", which three words cannot answer).
 */
public fun offersThreeWords(
    cachedUsername: String?,
    phraseOnDevice: Boolean,
    keysOnDevice: Boolean,
): Boolean =
    cachedUsername != null && cachedUsername.trim().isNotEmpty() && phraseOnDevice && keysOnDevice

/** Misses allowed before the route pauses. */
public const val THREE_WORDS_MAX_MISSES: Int = 5

/** How long the route pauses after the last allowed miss. */
public const val THREE_WORDS_PAUSE_MS: Long = 15L * 60L * 1000L

/** The budget for one account on this device. */
@Serializable
public data class ThreeWordsBudget(
    /** Misses since the last success or the last pause. */
    val misses: Int = 0,
    /** When the pause ends (unix ms), or `null` when the route is open. */
    val pausedUntilMs: Long? = null,
) {
    public companion object {
        public val FRESH: ThreeWordsBudget = ThreeWordsBudget()
    }
}

/** The budget as it stands at [nowMs]: a pause that has run out is over and
 *  the count starts again. */
public fun budgetAt(budget: ThreeWordsBudget, nowMs: Long): ThreeWordsBudget =
    if (budget.pausedUntilMs != null && nowMs >= budget.pausedUntilMs) ThreeWordsBudget.FRESH else budget

/** Milliseconds of pause left at [nowMs]; 0 when the route is open. A device
 *  clock set back cannot stretch a pause past its full length. */
public fun pauseRemainingMs(budget: ThreeWordsBudget, nowMs: Long): Long {
    val until = budgetAt(budget, nowMs).pausedUntilMs ?: return 0L
    return (until - nowMs).coerceIn(0L, THREE_WORDS_PAUSE_MS)
}

/** Tries left before the pause, at [nowMs] (0 while paused). */
public fun triesLeft(budget: ThreeWordsBudget, nowMs: Long): Int {
    val current = budgetAt(budget, nowMs)
    if (current.pausedUntilMs != null) return 0
    return (THREE_WORDS_MAX_MISSES - current.misses).coerceAtLeast(0)
}

/** The budget after one more miss at [nowMs]. The fifth miss starts the
 *  pause. A miss while paused does not happen (the form is closed) and
 *  changes nothing. */
public fun afterMiss(budget: ThreeWordsBudget, nowMs: Long): ThreeWordsBudget {
    val current = budgetAt(budget, nowMs)
    if (current.pausedUntilMs != null) return current
    val misses = current.misses + 1
    return if (misses >= THREE_WORDS_MAX_MISSES) {
        ThreeWordsBudget(misses = misses, pausedUntilMs = nowMs + THREE_WORDS_PAUSE_MS)
    } else {
        ThreeWordsBudget(misses = misses, pausedUntilMs = null)
    }
}

/** A wait as `m:ss` (Latin digits in every language, like the word numbers),
 *  rounded up to the next second so it never reads 0:00 while it runs. */
public fun formatCountdown(remainingMs: Long): String {
    val totalSecs = if (remainingMs <= 0L) 0L else (remainingMs + 999L) / 1000L
    return "${totalSecs / 60}:${(totalSecs % 60).toString().padStart(2, '0')}"
}

/**
 * Whether [word] has the shape of a recovery-phrase word: 3 to 8 lower-case
 * Latin letters after trimming and case folding. The Android client carries
 * no copy of the word list (the core owns it), so this is what enables the
 * form's button; a word of the right shape that is not on the list is then
 * simply a wrong answer.
 */
public fun looksLikePhraseWord(word: String): Boolean {
    val w = word.trim().lowercase()
    return w.length in 3..8 && w.all { it in 'a'..'z' }
}

// ---------------------------------------------------------------------------
// Persistence: per account, so a restart does not reset the budget
// ---------------------------------------------------------------------------

/** A small string store keyed by name (plain shared preferences in the app,
 *  a map in host tests). */
public interface MarkerStorage {
    public fun get(key: String): String?
    public fun set(key: String, value: String)
    public fun remove(key: String)
}

/** [MarkerStorage] over one plain `SharedPreferences` file. Plain on
 *  purpose: the rows hold counters and flags, no secret. */
public class SharedPrefsMarkerStorage(context: Context, name: String) : MarkerStorage {
    private val prefs = context.applicationContext.getSharedPreferences(name, Context.MODE_PRIVATE)

    override fun get(key: String): String? = prefs.getString(key, null)

    override fun set(key: String, value: String) {
        prefs.edit().putString(key, value).commit()
    }

    override fun remove(key: String) {
        prefs.edit().remove(key).commit()
    }
}

/** The three-word budget of each account on this device. Cleared on a
 *  success, on Forget device and on account deletion. */
public class ThreeWordsBudgetStore(private val storage: MarkerStorage) {

    /** The stored budget, or a fresh one when nothing (or nothing readable)
     *  is stored. A malformed row reads as fresh rather than locking the
     *  route for good; a pause that lies further ahead than a full pause (a
     *  clock that was set back) is clamped by [pauseRemainingMs]. */
    public fun load(accountIdB64: String): ThreeWordsBudget {
        val raw = try {
            storage.get(key(accountIdB64))
        } catch (_: Throwable) {
            null
        } ?: return ThreeWordsBudget.FRESH
        val parsed = try {
            json.decodeFromString(ThreeWordsBudget.serializer(), raw)
        } catch (_: Throwable) {
            return ThreeWordsBudget.FRESH
        }
        if (parsed.misses < 0) return ThreeWordsBudget.FRESH
        return parsed.copy(misses = parsed.misses.coerceAtMost(THREE_WORDS_MAX_MISSES))
    }

    public fun save(accountIdB64: String, budget: ThreeWordsBudget) {
        try {
            storage.set(key(accountIdB64), json.encodeToString(ThreeWordsBudget.serializer(), budget))
        } catch (_: Throwable) {
            // Storage refused: the form's in-memory copy still holds.
        }
    }

    public fun clear(accountIdB64: String) {
        try {
            storage.remove(key(accountIdB64))
        } catch (_: Throwable) {
            // Nothing was recorded either.
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-three-words-budget"

        private val json = Json { ignoreUnknownKeys = true }

        internal fun key(accountIdB64: String): String = "budget:$accountIdB64"

        public fun onDevice(context: Context): ThreeWordsBudgetStore =
            ThreeWordsBudgetStore(SharedPrefsMarkerStorage(context, PREFS_NAME))
    }
}

// ---------------------------------------------------------------------------
// What the sign-in after a match can fail with
// ---------------------------------------------------------------------------

/**
 * How the form reads a failure of the sign-in that follows a MATCH. None of
 * these is a wrong guess, so none charges the budget.
 */
public sealed interface ThreeWordsFailure {
    /** A server limit with a stated wait: a countdown, the button disabled. */
    public data class RateLimited(val retryAfterSecs: Long) : ThreeWordsFailure

    /** The route is switched off on this server: back to the list. */
    public data object Unavailable : ThreeWordsFailure

    /** The phrase stored here is no longer the account's recovery
     *  credential (a new identity was created on another device). */
    public data object Rejected : ThreeWordsFailure

    public data object Offline : ThreeWordsFailure

    /** A sentence the failure carries itself (a limit without a wait, the
     *  route not offered here). */
    public data class Sentence(val text: me.lakko.app.l10n.UiText) : ThreeWordsFailure

    public data object Generic : ThreeWordsFailure
}

/** The wait a server limit asks for, in seconds, or `null` when [err] is not
 *  an attempts limit with a retry time. */
public fun attemptsRetryAfterSecs(err: Throwable): Long? {
    if (err !is AuthHttpError || err.status != 429) return null
    if (err.code != null && err.code != "too-many-attempts") return null
    return err.retryAfterSecs?.takeIf { it > 0 }
}

public fun threeWordsFailureOf(err: Throwable): ThreeWordsFailure {
    attemptsRetryAfterSecs(err)?.let { return ThreeWordsFailure.RateLimited(it) }
    return when {
        err is AuthHttpError && err.status == 503 && err.code != "rate-limiter-unavailable" ->
            ThreeWordsFailure.Unavailable
        err is AuthHttpError && (err.status == 401 || err.status == 404) -> ThreeWordsFailure.Rejected
        err is AuthNetworkError -> ThreeWordsFailure.Offline
        err is AuthHttpError -> authLimitText(err)?.let { ThreeWordsFailure.Sentence(it) } ?: ThreeWordsFailure.Generic
        err is AuthClientArgError -> err.text?.let { ThreeWordsFailure.Sentence(it) } ?: ThreeWordsFailure.Generic
        else -> ThreeWordsFailure.Generic
    }
}
