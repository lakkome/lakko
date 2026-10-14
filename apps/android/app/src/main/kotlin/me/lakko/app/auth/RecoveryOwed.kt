// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.content.Context

/**
 * Two debts a landing can leave behind for an account on this device, kept
 * in plain storage so they survive a restart (flags only, no secret):
 *
 *   - **phrase owed**: a recovery phrase was generated on this device (a
 *     sign-up or a fresh identity) and the person has not yet confirmed
 *     they saved it. The next session for that account shows the phrase
 *     first, read back from the phrase store.
 *   - **publish owed** (ADR-0073 section 2.3, the successor of the
 *     recovery-credential debt): the account keys were reset on this device
 *     (the server step is done: the new seed and phrase are stored here, the
 *     other devices are signed out) but the new identity is not yet
 *     published to the directory and the device list. Until it is, the
 *     contacts and the other devices cannot find the new keys, so the new
 *     words are not shown; the next session landing retries the publish in
 *     the background with the stored attestation ([KeyResetOwedStore]).
 *
 * Mirror of the web `phrase-gate.ts` and `publish-owed.ts` markers. Cleared
 * on Forget device and on account deletion.
 */
public class RecoveryOwedStore(private val storage: MarkerStorage) {

    public fun isPhraseOwed(accountIdB64: String): Boolean = has(PHRASE_PREFIX + accountIdB64)

    /** Record that a phrase is owed, and where it came from, so a restart
     *  shows it with the right words around it. */
    public fun markPhraseOwed(accountIdB64: String, origin: OwedPhraseOrigin): Unit =
        put(accountIdB64, PHRASE_PREFIX + accountIdB64, origin.tag)

    /**
     * Where the owed phrase of [accountIdB64] came from, or `null` when none
     * is owed (or storage cannot be read). A row written before the origin
     * was recorded, or with a value this build does not know, reads as
     * [OwedPhraseOrigin.Signup].
     */
    public fun owedPhraseOrigin(accountIdB64: String): OwedPhraseOrigin? {
        val raw = try {
            storage.get(PHRASE_PREFIX + accountIdB64)
        } catch (_: Throwable) {
            null
        } ?: return null
        return OwedPhraseOrigin.entries.firstOrNull { it.tag == raw } ?: OwedPhraseOrigin.Signup
    }

    public fun clearPhraseOwed(accountIdB64: String): Unit = drop(PHRASE_PREFIX + accountIdB64)

    public fun isPublishOwed(accountIdB64: String): Boolean = has(PUBLISH_PREFIX + accountIdB64)

    public fun markPublishOwed(accountIdB64: String): Unit = put(accountIdB64, PUBLISH_PREFIX + accountIdB64, "1")

    public fun clearPublishOwed(accountIdB64: String): Unit = drop(PUBLISH_PREFIX + accountIdB64)

    /** Both debts of [accountIdB64] (Forget device, account deletion). */
    public fun clearAccount(accountIdB64: String) {
        clearPhraseOwed(accountIdB64)
        clearPublishOwed(accountIdB64)
    }

    /**
     * Whether the phrase may be shown now: it is owed, and no publish of a
     * reset's new keys is outstanding (a phrase for an identity nobody can
     * find is not yet a recovery phrase, so it is held back).
     */
    public fun phraseShowable(accountIdB64: String): Boolean =
        isPhraseOwed(accountIdB64) && !isPublishOwed(accountIdB64)

    private fun has(key: String): Boolean = try {
        storage.get(key) != null
    } catch (_: Throwable) {
        false
    }

    private fun put(accountIdB64: String, key: String, value: String) {
        try {
            // Refused for a signed-out or wiped account: no late write puts
            // a debt back ([me.lakko.app.account.AccountStoreGate]).
            me.lakko.app.account.AccountStoreGate.write(accountIdB64) { storage.set(key, value) }
        } catch (_: Throwable) {
            // The debt then lives for this process only.
        }
    }

    private fun drop(key: String) {
        try {
            storage.remove(key)
        } catch (_: Throwable) {
            // Nothing to drop.
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-recovery-owed"
        private const val PHRASE_PREFIX: String = "phrase-owed:"
        private const val PUBLISH_PREFIX: String = "publish-owed:"

        public fun onDevice(context: Context): RecoveryOwedStore =
            RecoveryOwedStore(SharedPrefsMarkerStorage(context, PREFS_NAME))
    }
}

/** Where a phrase that is still owed to the person came from. */
public enum class OwedPhraseOrigin(public val tag: String) {
    /** A sign-up (with a username or as a guest): the account's first phrase. */
    Signup("signup"),

    /** "Start over with a new identity": a phrase that replaced an older one. */
    FreshIdentity("fresh"),
}

/** What "Show recovery phrase" in Settings does. */
public enum class PhraseRevealGate {
    /** The publish of a reset's new keys is owed: the words stored here are
     *  not a recovery phrase anybody can use yet, so they are withheld and
     *  "Try again" is offered. */
    PublishPending,

    /** This device holds no phrase. */
    NotSaved,

    /** Show the words (after the device's own authentication). */
    Show,
}

/**
 * The rule of Settings > Security > Recovery > "Show recovery phrase": the
 * words are withheld while the publish of a reset is owed, whatever else is
 * true, and shown only once nothing is owed.
 */
public fun phraseRevealGate(publishOwed: Boolean, phraseOnDevice: Boolean): PhraseRevealGate = when {
    publishOwed -> PhraseRevealGate.PublishPending
    !phraseOnDevice -> PhraseRevealGate.NotSaved
    else -> PhraseRevealGate.Show
}
