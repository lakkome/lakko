// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

import me.lakko.app.discovery.DiscoveryClient
import me.lakko.app.discovery.DiscoveryHttpError

/**
 * The username half of the discovery directory, as the auth flows use it.
 *
 * ## The defect this closes
 *
 * `DiscoveryClient.registerUsername` had no call site on Android. A user who
 * signed up here was never bound to their `@username`, so nobody could find
 * them to add them as a contact or to a group, and they could not sign in on a
 * second device, because every username-keyed flow resolves through
 * `/discovery/lookup` and got a 404. Login made the second half worse: it
 * refused outright when the device held no cached account, where web resolves
 * the typed username through discovery.
 *
 * Narrowed to an interface so the rules below — which account a sign-in
 * targets, what a taken name looks like to the user — are unit-testable. The
 * production implementation drives the OPRF flow, which needs the native core.
 *
 * ## Normalisation
 *
 * Nothing here normalises a username. The canonical form (trim, strip a
 * leading `@`, NFKC, lowercase) is computed inside the native OPRF blinder
 * `DiscoveryClient` already uses, and handed back as the normalised username;
 * a second Kotlin normaliser could only drift from the Rust one.
 */
internal interface AccountDirectory {
    /**
     * Bind [username] to [accountId] under [sessionToken] (the server refuses a
     * body account id that does not match the token's).
     *
     * @return the normalised username the server actually bound.
     * @throws DiscoveryHttpError with `status == 409` when the name is taken.
     */
    suspend fun registerUsername(sessionToken: String, accountId: ByteArray, username: String): String

    /** Resolve [username] to its account; `account == null` when unregistered. */
    suspend fun lookup(username: String): RecoverAccountLookup
}

/** [AccountDirectory] over a short-lived [DiscoveryClient] per call. */
internal class DiscoveryAccountDirectory(
    private val clientFactory: () -> DiscoveryClient,
) : AccountDirectory {
    override suspend fun registerUsername(
        sessionToken: String,
        accountId: ByteArray,
        username: String,
    ): String = clientFactory().use { client ->
        // A fresh client per call, so the bearer is never left attached to a
        // client some other flow reuses.
        client.setSessionToken(sessionToken)
        client.registerUsername(accountId, username).normalisedUsername
    }

    override suspend fun lookup(username: String): RecoverAccountLookup =
        clientFactory().use { client ->
            val result = client.lookup(username)
            RecoverAccountLookup(
                account = result.account,
                normalisedUsername = result.normalisedUsername,
            )
        }
}

/**
 * Claim [username] for the account signup just created. Mirror of the web
 * `signup` (`apps/web/src/auth/AuthContext.tsx`), which does this right after
 * the OPAQUE registration commits and before anything is persisted locally.
 *
 * A taken name becomes an [AuthClientArgError] carrying the sentence the form
 * shows; every other failure propagates unchanged, so a network error still
 * reads as one. Either way signup fails rather than landing a session nobody
 * can find — the account then exists server-side with no username, the same
 * posture web accepts (the user retries with another name).
 *
 * @return the normalised username to persist and display.
 */
internal suspend fun claimUsernameAtSignup(
    directory: AccountDirectory,
    sessionToken: String,
    accountId: ByteArray,
    username: String,
): String {
    val trimmed = username.trim()
    try {
        return directory.registerUsername(sessionToken, accountId, trimmed)
    } catch (err: DiscoveryHttpError) {
        if (err.status == HTTP_CONFLICT) {
            throw AuthClientArgError("username @$trimmed is already taken — pick another")
        }
        throw err
    }
}

/** The account a password sign-in targets. */
internal data class ResolvedLoginAccount(
    val accountIdB64: String,
    /** The username to persist and show: the cached one, or the normalised form discovery returned. */
    val username: String,
    /** `true` when the on-device cache answered and no lookup was made. */
    val fromCache: Boolean,
)

/**
 * Resolve the typed [username] to the account a password login targets. Mirror
 * of the web `login`:
 *
 *   * the on-device marker answers when its username equals the typed one —
 *     no round trip;
 *   * otherwise (no marker, or a different name) discovery resolves it, which
 *     is what makes signing in on a second device possible;
 *   * an unregistered name fails exactly like a wrong password (401
 *     `invalid credentials`), so the sign-in form does not become a tool for
 *     telling registered names from unregistered ones.
 *
 * This only picks the account. The OPAQUE login and the ADR-0017 identity
 * acquisition that follow are unchanged: a device with no cached phrase seed
 * still lands in `NeedsRecoveryPhrase`.
 */
internal suspend fun resolveLoginAccount(
    cached: CachedAccount?,
    username: String,
    directory: AccountDirectory,
    encodeAccountId: (ByteArray) -> String,
): ResolvedLoginAccount {
    val trimmed = username.trim()
    if (cached != null && cached.username == trimmed) {
        return ResolvedLoginAccount(
            accountIdB64 = cached.accountIdB64,
            username = cached.username,
            fromCache = true,
        )
    }
    val result = directory.lookup(trimmed)
    val account = result.account
        ?: throw AuthHttpError(
            status = 401,
            endpoint = "/discovery/lookup",
            body = "invalid credentials",
        )
    return ResolvedLoginAccount(
        accountIdB64 = encodeAccountId(account),
        username = result.normalisedUsername,
        fromCache = false,
    )
}

private const val HTTP_CONFLICT = 409
