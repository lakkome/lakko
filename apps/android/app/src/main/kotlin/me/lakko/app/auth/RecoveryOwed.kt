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
 *   - **recovery credential owed**: a fresh identity was published and its
 *     seed stored here, but the server's recovery credential still belongs
 *     to the OLD phrase (the replace failed). Until it is replaced the new
 *     words would not sign in, so they are not shown; the next session
 *     landing retries the replace in the background.
 *
 * Mirror of the web `phrase-gate.ts` and `recovery-credential-owed.ts`
 * markers. Cleared on Forget device and on account deletion.
 */
public class RecoveryOwedStore(private val storage: MarkerStorage) {

    public fun isPhraseOwed(accountIdB64: String): Boolean = has(PHRASE_PREFIX + accountIdB64)

    public fun markPhraseOwed(accountIdB64: String): Unit = put(PHRASE_PREFIX + accountIdB64)

    public fun clearPhraseOwed(accountIdB64: String): Unit = drop(PHRASE_PREFIX + accountIdB64)

    public fun isCredentialOwed(accountIdB64: String): Boolean = has(CREDENTIAL_PREFIX + accountIdB64)

    public fun markCredentialOwed(accountIdB64: String): Unit = put(CREDENTIAL_PREFIX + accountIdB64)

    public fun clearCredentialOwed(accountIdB64: String): Unit = drop(CREDENTIAL_PREFIX + accountIdB64)

    /** Both debts of [accountIdB64] (Forget device, account deletion). */
    public fun clearAccount(accountIdB64: String) {
        clearPhraseOwed(accountIdB64)
        clearCredentialOwed(accountIdB64)
    }

    /**
     * Whether the phrase may be shown now: it is owed, and no recovery
     * credential replace is outstanding (a phrase whose credential is not in
     * place yet signs in nowhere, so it is held back).
     */
    public fun phraseShowable(accountIdB64: String): Boolean =
        isPhraseOwed(accountIdB64) && !isCredentialOwed(accountIdB64)

    private fun has(key: String): Boolean = try {
        storage.get(key) != null
    } catch (_: Throwable) {
        false
    }

    private fun put(key: String) {
        try {
            storage.set(key, "1")
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
        private const val CREDENTIAL_PREFIX: String = "credential-owed:"

        public fun onDevice(context: Context): RecoveryOwedStore =
            RecoveryOwedStore(SharedPrefsMarkerStorage(context, PREFS_NAME))
    }
}
