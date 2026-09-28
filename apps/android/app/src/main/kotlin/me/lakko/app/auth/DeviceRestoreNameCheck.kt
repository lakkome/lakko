// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import android.util.Log
import kotlinx.coroutines.CancellationException

/*
 * ADR-0056 security pass. On the join-by-name restore (another device, no
 * password) the account the fresh device lands in comes from the grant
 * redeem, and the request was routed there by the auth login handle of the
 * typed name. A squatted login handle can route it to another account whose
 * device confirms; the fresh device would then land in that account under
 * the typed name.
 *
 * Discovery is a registry auth does not control (its own service, database
 * and OPRF key), so the fresh device asks it too: with the freshly redeemed
 * session it looks the typed name up through the ordinary token-spending
 * lookup and requires the resolved account to be the redeemed one. Only then
 * does anything land. Mirror of `apps/web/src/auth/device-restore-name-check.ts`.
 */

/** The typed name resolves, at discovery, to another account or to none. */
public const val TYPED_NAME_OTHER_ACCOUNT_COPY: String =
    "The name you typed belongs to a different account. Nothing was changed on this device."

/** The lookup could not run; the generic network failure copy. */
public const val TYPED_NAME_CHECK_UNAVAILABLE_COPY: String =
    "Could not reach Lakko. Check your connection and try again."

/** The typed name does not resolve to the redeemed account; nothing landed. */
public class TypedNameOtherAccount : Exception(TYPED_NAME_OTHER_ACCOUNT_COPY)

/** The typed name could not be checked (network, token quota, tokens
 *  switched off); nothing landed. */
public class TypedNameCheckUnavailable(cause: Throwable) :
    Exception(TYPED_NAME_CHECK_UNAVAILABLE_COPY, cause)

/**
 * Run the gates of a join-by-name device restore, then write and land it.
 *
 * Order: [verifySeed] (the transferred seed reproduces the published identity
 * of the redeemed account) -> [lookup] of [username] with the redeemed session
 * -> compare with [accountIdBytes] -> [persistJoin] (the pairing's held state)
 * -> [land] (this device's own writes and the session). A refusal at any gate
 * zeroes [phraseSeed] and throws before [persistJoin] or [land] runs, so
 * nothing is written and the session is never installed.
 *
 * @throws TypedNameOtherAccount on a mismatch or a name discovery does not know.
 * @throws TypedNameCheckUnavailable when the lookup could not run.
 */
public suspend fun runCheckedDeviceRestore(
    username: String,
    accountIdBytes: ByteArray,
    phraseSeed: ByteArray,
    verifySeed: suspend () -> Unit,
    lookup: suspend (username: String) -> ByteArray?,
    persistJoin: suspend () -> Unit,
    land: suspend () -> Unit,
    log: (String) -> Unit = { Log.w("lakko-auth", it) },
) {
    try {
        verifySeed()
    } catch (e: Throwable) {
        phraseSeed.fill(0)
        throw e
    }
    val resolved = try {
        lookup(username)
    } catch (e: CancellationException) {
        phraseSeed.fill(0)
        throw e
    } catch (e: Throwable) {
        phraseSeed.fill(0)
        log("device restore: typed-name lookup failed; refusing (${e.javaClass.simpleName})")
        throw TypedNameCheckUnavailable(e)
    }
    if (resolved == null || !constantTimeEquals(resolved, accountIdBytes)) {
        phraseSeed.fill(0)
        log(
            if (resolved == null) {
                "device restore: typed name unknown to discovery; refusing"
            } else {
                "device restore: typed name resolves to another account; refusing"
            },
        )
        throw TypedNameOtherAccount()
    }
    persistJoin()
    land()
}

private fun constantTimeEquals(a: ByteArray, b: ByteArray): Boolean {
    if (a.size != b.size) return false
    var diff = 0
    for (i in a.indices) diff = diff or (a[i].toInt() xor b[i].toInt())
    return diff == 0
}
