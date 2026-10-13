// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

/*
 * A signup's recovery phrase is shown and acknowledged before any other
 * screen, also after the app was closed in between. Android twin of the
 * marker half of `apps/web/src/auth/phrase-gate.ts`.
 *
 * The phrase screen itself is driven by the in-memory
 * [me.lakko.app.ui.auth.SignupPhraseGate]. This marker is what survives the
 * process: it records only that an account still owes the phrase screen. The
 * phrase is NOT written here; it is already kept in the Keystore-backed
 * [me.lakko.app.backup.PhraseStore] under the account's key, and a restart
 * reads it back from there ([owedSignupPhrase]).
 *
 * The same marker covers the phrase of a fresh identity
 * (`AuthRepository.mintFreshIdentity`). It is cleared when the person confirms
 * they saved the phrase, and by forget-device; a sign-out keeps it, so an
 * unconfirmed phrase is shown again at the account's next sign-in.
 */

/** The preferences file of the owed marker. */
public const val SIGNUP_PHRASE_OWED_PREFS: String = "lakko-signup-phrase-owed"

/** The marker's key for [accountIdB64] in [SIGNUP_PHRASE_OWED_PREFS]. */
public fun signupPhraseOwedKey(accountIdB64: String): String = "account:$accountIdB64"

private const val OWED = "1"

/** Record that [accountIdB64] has a signup phrase the person has not yet
 *  acknowledged. Never throws. */
public fun markSignupPhraseOwed(storage: MarkerStorage?, accountIdB64: String) {
    try {
        storage?.setItem(signupPhraseOwedKey(accountIdB64), OWED)
    } catch (_: Exception) {
        // Storage refused: the in-memory gate still holds for this process.
    }
}

/** Whether a restart of [accountIdB64] still owes the phrase screen. Never
 *  throws. */
public fun isSignupPhraseOwed(storage: MarkerStorage?, accountIdB64: String): Boolean =
    try {
        storage?.getItem(signupPhraseOwedKey(accountIdB64)) == OWED
    } catch (_: Exception) {
        false
    }

/** Clear the marker: on acknowledgement, sign-out and device wipe. Never
 *  throws. */
public fun clearSignupPhraseOwed(storage: MarkerStorage?, accountIdB64: String) {
    try {
        storage?.removeItem(signupPhraseOwedKey(accountIdB64))
    } catch (_: Exception) {
        // Storage refused: nothing was recorded either.
    }
}

/**
 * The phrase a restored session still owes its person, or `null`.
 *
 * `null` when nothing is owed, and also when the phrase is owed but not
 * readable on this device ([readPhrase] answered `null` or threw): it cannot
 * be shown again, which is reported through [log] rather than blocking the
 * account forever.
 */
public fun owedSignupPhrase(
    storage: MarkerStorage?,
    accountIdB64: String,
    readPhrase: () -> String?,
    log: (String, Throwable?) -> Unit,
): String? {
    if (!isSignupPhraseOwed(storage, accountIdB64)) return null
    val phrase = try {
        readPhrase()
    } catch (cause: Exception) {
        log("reading the owed signup recovery phrase failed", cause)
        return null
    }
    if (phrase == null) {
        log("signup recovery phrase is owed but not readable on this device; it cannot be shown again", null)
    }
    return phrase
}
