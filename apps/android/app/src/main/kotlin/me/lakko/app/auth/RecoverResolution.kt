// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

/**
 * Resolve which account a recover-from-words flow should target.
 *
 * Extracted from [AuthRepository.recover] (G-29) as a pure,
 * dependency-injected function so the cross-device behaviour — a fresh
 * device recovers given the username + phrase — is locked by a unit
 * test instead of living inlined in a large `suspend` method where it
 * can silently regress. Mirror of the web `resolveRecoverAccount`
 * (`apps/web/src/auth/recover-resolution.ts`) + the iOS twin.
 *
 * The rule (identical on web / iOS / Android `recover`): a local
 * `(accountId, username)` cache hit wins when the typed username is
 * empty or matches it; otherwise a discovery lookup on the typed
 * username binds the account. With neither we cannot target the
 * recover. A lookup miss collapses into the same error a bad phrase
 * yields downstream — we never confirm or deny a username's existence
 * (preserves the no-probing property).
 */

/** Minimal shape of a locally-cached `(accountId, username)` record. */
public data class CachedAccount(
    val accountIdB64: String,
    val username: String,
)

/** Minimal shape of a discovery account lookup result. */
public data class RecoverAccountLookup(
    /** Raw 16-byte account id, or `null` when the username is unregistered. */
    val account: ByteArray?,
    /** Server-normalised form of the looked-up username. */
    val normalisedUsername: String,
)

/** The resolved account the recover targets. */
public data class ResolvedRecoverAccount(
    val accountIdB64: String,
    val resolvedUsername: String,
)

/**
 * Resolve the account to recover.
 *
 * @param cached the locally-cached `(accountId, username)` marker, or
 *        `null` on a fresh device with no prior session.
 * @param username the username typed into the recover form, or `null`
 *        when none was entered.
 * @param lookup discovery resolution of a typed username — injected so
 *        the helper is unit-testable without a network.
 * @param encodeAccountId base64 encoder for the looked-up account bytes.
 *
 * @throws AuthClientArgError `"invalid recovery"` when the typed
 *         username is unregistered (collapsed with the downstream
 *         bad-phrase rejection — no username probing).
 * @throws AuthClientArgError a fresh-device prompt when neither a
 *         cached account nor a typed username is available.
 */
public suspend fun resolveRecoverAccount(
    cached: CachedAccount?,
    username: String?,
    lookup: suspend (String) -> RecoverAccountLookup,
    encodeAccountId: (ByteArray) -> String,
): ResolvedRecoverAccount {
    val trimmedUsername = username?.trim().orEmpty()

    // A local cache hit wins when the typed username is empty or matches it.
    if (cached != null &&
        (trimmedUsername.isEmpty() || cached.username == trimmedUsername)
    ) {
        return ResolvedRecoverAccount(
            accountIdB64 = cached.accountIdB64,
            resolvedUsername = cached.username,
        )
    }

    // Otherwise resolve the typed username via discovery so a FRESH
    // device can recover given username + phrase.
    if (trimmedUsername.isNotEmpty()) {
        val result = lookup(trimmedUsername)
        val account = result.account
            ?: // Collapsed with "bad phrase" downstream by the OPAQUE
            // rejection — we don't help probe the username space.
            throw AuthClientArgError("invalid recovery")
        return ResolvedRecoverAccount(
            accountIdB64 = encodeAccountId(account),
            resolvedUsername = result.normalisedUsername,
        )
    }

    throw AuthClientArgError(
        "no account on this device — enter your username so we " +
            "can look up the account to recover",
    )
}
