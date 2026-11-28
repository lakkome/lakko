// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.webauthn

import android.content.Context
import android.util.Log
import me.lakko.app.auth.AuthRepository
import me.lakko.app.auth.AuthState
import me.lakko.app.auth.base64Encode
import me.lakko.app.devices.DeviceRemoval

/*
 * Adding a passkey (docs/ux/identity-recovery.md 10.2, amended 2026-11-11):
 * every passkey restores keys where the device and the passkey provider can
 * carry them. There is no choice: once the system sheet created the passkey
 * the keys are stored for it, from the creation's PRF output or one more
 * sheet for that passkey. Used by Settings > Security > Passkeys and by the
 * passkey offer at the end of sign-up.
 */

/** A passkey that was created, and how storing its keys ended. */
public class PasskeyAddResult(
    /** base64url, no padding. */
    public val credentialIdB64: String,
    public val keys: PasskeyKeysStoreEnd,
    /** The passkey's authenticator gave a secret of its own and the
     *  account's published protection now lists it: this passkey keeps the
     *  chats after a key reset (UX 19.1d, ADR-0101 section 10). */
    public val keepsChats: Boolean = false,
)

/**
 * Create a passkey for the signed-in account and, now that the account has
 * one, turn on attested device removals. Nothing is stored for the passkey
 * beyond the credential; [addPasskeyWithKeys] stores the keys for it next.
 *
 * @throws WebauthnUserCancelledError when the sheet was dismissed.
 * @throws me.lakko.app.auth.FreshAuthRequired when no fresh-auth proof is
 *   live, or the server refused the one sent. Nothing was added.
 */
public suspend fun addPasskeyForSession(
    context: Context,
    repo: AuthRepository,
    nickname: String,
    client: WebauthnClient = WebauthnClient(),
    registration: WebauthnRegistration = WebauthnRegistration(),
): PasskeyAdded {
    val session = repo.state.value as? AuthState.Session
    val secrets = repo.identitySecrets.value
    check(session != null && secrets != null) { "no session" }
    client.setSessionToken(secrets.sessionToken)
    // UX section 17: a session alone may not add a passkey. The caller shows
    // the proof block first; with no live proof nothing is sent.
    val freshGrant = repo.freshGrant() ?: throw me.lakko.app.auth.FreshAuthRequired()
    val added = try {
        client.registerPasskey(context, session.account.username, nickname, freshGrant, registration)
    } catch (cause: WebauthnHttpError) {
        if (me.lakko.app.auth.isFreshAuthRequired(cause)) {
            // The proof ran out between the block and the call.
            repo.dropFreshProof()
            throw me.lakko.app.auth.FreshAuthRequired()
        }
        throw cause
    }
    // G-13 (ADR-0015): the account has a passkey now, so device removals
    // require a peer-verified gesture. Non-fatal.
    try {
        DeviceRemoval.enableAttestedRemovalsIfNeeded(
            context = context,
            accountIdBytes = secrets.accountId,
            accountIdB64 = base64Encode(secrets.accountId),
        )
    } catch (cause: Throwable) {
        Log.w("lakko-passkeys", "failed to enable attested removals: ${cause.javaClass.simpleName}")
    }
    return added
}

/**
 * Create a passkey ([addPasskeyForSession]) and store the account's keys for
 * it with nothing to choose ([storeKeysForAddedPasskey]). [onAdded] is told
 * once the passkey exists, before the keys are stored. A passkey this device
 * learned cannot carry keys is remembered ([PasskeyNoPrfStore]), so its row
 * offers no repair.
 *
 * Throws only what [addPasskeyForSession] throws: once the passkey exists,
 * every outcome is a [PasskeyAddResult].
 */
public suspend fun addPasskeyWithKeys(
    context: Context,
    repo: AuthRepository,
    nickname: String,
    client: WebauthnClient = WebauthnClient(),
    registration: WebauthnRegistration = WebauthnRegistration(),
    onAdded: () -> Unit = {},
): PasskeyAddResult {
    val added = addPasskeyForSession(context, repo, nickname, client, registration)
    onAdded()
    // ADR-0101 section 10: a passkey that gave a seal secret is announced,
    // and its owned-channels lock made. Below API 34 the request is dropped
    // at creation, so such a passkey confirms a reset and never seals one.
    val keepsChats = try {
        repo.adoptPasskeySeal(added.credentialIdB64, added.sealPrfOutput)
    } finally {
        added.sealPrfOutput?.fill(0)
    }
    // The account's factors changed: the screens read them again.
    repo.refreshAccountFactors()
    val keys = try {
        storeKeysForAddedPasskey(
            prfOutput = added.prfOutput,
            prfEnabled = added.prfEnabled,
            onFailure = { cause ->
                Log.w("lakko-passkeys", "storing keys for a new passkey failed: ${cause.javaClass.simpleName}")
            },
        ) { registrationPrf ->
            repo.turnOnPasskeyKeys(
                activityContext = context,
                credentialIdB64 = added.credentialIdB64,
                registrationPrfOutput = registrationPrf,
                typedPhrase = null,
            )
        }
    } finally {
        added.prfOutput?.fill(0)
    }
    if (keys == PasskeyKeysStoreEnd.NoPrf) {
        (repo.state.value as? AuthState.Session)?.let { session ->
            PasskeyNoPrfStore.record(context, session.account.accountIdB64, added.credentialIdB64)
        }
    }
    return PasskeyAddResult(added.credentialIdB64, keys, keepsChats)
}

/**
 * The account's passkeys this device learned cannot carry keys (no PRF
 * output on this device and passkey provider), by credential id. Their rows
 * read "Logs in only" with no "Restore keys with this passkey": the repair
 * would only say again that it cannot. Per account; Forget device deletes it.
 */
public object PasskeyNoPrfStore {
    private const val PREFS = "lakko-passkeys-no-prf"

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    /** The credential ids recorded for [accountIdB64]. */
    public fun of(context: Context, accountIdB64: String): Set<String> =
        prefs(context).getStringSet(accountIdB64, null)?.toSet() ?: emptySet()

    /** [credentialIdB64] cannot carry keys on this device. */
    @Synchronized
    public fun record(context: Context, accountIdB64: String, credentialIdB64: String) {
        val now = of(context, accountIdB64) + credentialIdB64
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs(context).edit().putStringSet(accountIdB64, now).apply()
        }
    }

    /** Keep only the ids of passkeys the account still has ([current]). */
    @Synchronized
    public fun retain(context: Context, accountIdB64: String, current: Set<String>) {
        val before = of(context, accountIdB64)
        val after = before intersect current
        if (after == before) return
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs(context).edit().putStringSet(accountIdB64, after).apply()
        }
    }

    /** Forget device. */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        prefs(context).edit().remove(accountIdB64).apply()
    }
}
