// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

/**
 * ADR-0065 section 3 — the session pair every session-landing response
 * carries: a short-lived access token (the bearer of every service), the
 * refresh credential only `POST /auth/session/refresh` accepts, and the key
 * generation both were issued under. Treated as opaque strings; the
 * generation is what tells a device that its keys are stale.
 *
 * Never printed: [toString] redacts both credentials.
 */
public class SessionPair(
    /** `v2.<account>.<generation>.<expiry>.<tag>` - the bearer. */
    public val accessToken: String,
    /** Unix milliseconds; the access token is unusable at or after this. */
    public val accessExpiresAtMs: Long,
    /** `r1.<account>.<generation>.<expiry>.<tag>` - refresh only. */
    public val refreshToken: String,
    /** Unix milliseconds; slides on every refresh. */
    public val refreshExpiresAtMs: Long,
    /** The account's key generation the pair belongs to (1 = never reset). */
    public val keyGeneration: Long,
) {
    override fun equals(other: Any?): Boolean {
        if (this === other) return true
        if (other !is SessionPair) return false
        return accessToken == other.accessToken &&
            accessExpiresAtMs == other.accessExpiresAtMs &&
            refreshToken == other.refreshToken &&
            refreshExpiresAtMs == other.refreshExpiresAtMs &&
            keyGeneration == other.keyGeneration
    }

    override fun hashCode(): Int {
        var h = accessToken.hashCode()
        h = 31 * h + accessExpiresAtMs.hashCode()
        h = 31 * h + refreshToken.hashCode()
        h = 31 * h + refreshExpiresAtMs.hashCode()
        h = 31 * h + keyGeneration.hashCode()
        return h
    }

    override fun toString(): String =
        "SessionPair(access=<redacted>, accessExpiresAtMs=$accessExpiresAtMs, " +
            "refresh=<redacted>, refreshExpiresAtMs=$refreshExpiresAtMs, keyGeneration=$keyGeneration)"
}

/** The `error` code a `401` from the refresh route or any bearer route of
 *  auth or discovery carries when the account's keys were reset elsewhere. */
public const val KEY_GENERATION_SUPERSEDED: String = "key-generation-superseded"

/** The refresh route's `401` for a bad, expired or unknown credential. */
public const val REFRESH_INVALID: String = "refresh-invalid"

/**
 * What a `401` means for this device (ADR-0065 section 3.3 and 3.5; the
 * contract's section 7). Pure over the status and the body's `error` code.
 */
public enum class SessionRefusal {
    /** `key-generation-superseded`: the account's keys were reset on another
     *  device. This device signs out and wipes; nothing is retried. */
    KeysReset,

    /** `refresh-invalid`: the refresh credential is gone (expired, unknown).
     *  The device goes to the sign-in screen; nothing is wiped. */
    RefreshInvalid,

    /** Any other `401`: the access token is expired or wrong. One refresh,
     *  one retry. */
    Unauthorized,
}

/** Classify a response; `null` when it is not a `401`. [errorCode] is the
 *  body's `error` field, or `null` when the body carried none. */
public fun sessionRefusalOf(status: Int, errorCode: String?): SessionRefusal? {
    if (status != HTTP_UNAUTHORIZED_STATUS) return null
    return when (errorCode) {
        KEY_GENERATION_SUPERSEDED -> SessionRefusal.KeysReset
        REFRESH_INVALID -> SessionRefusal.RefreshInvalid
        else -> SessionRefusal.Unauthorized
    }
}

/** Refresh when this much, or less, of the access token's life is left. */
public const val SESSION_REFRESH_AHEAD_MS: Long = 120_000L

/** What the token provider does before handing out the access token. */
public enum class TokenAction {
    /** More than [SESSION_REFRESH_AHEAD_MS] remain: use it as it is. */
    Use,

    /** The token is about to expire or has expired: refresh first. */
    Refresh,
}

/** The pure decision of [SessionTokenProvider.current]. */
public fun tokenActionAt(accessExpiresAtMs: Long, nowMs: Long): TokenAction =
    if (accessExpiresAtMs - nowMs <= SESSION_REFRESH_AHEAD_MS) TokenAction.Refresh else TokenAction.Use

/** The version prefix of an access token. */
public const val ACCESS_TOKEN_PREFIX: String = "v2."

/** The version prefix of a refresh credential. */
public const val REFRESH_TOKEN_PREFIX: String = "r1."

/**
 * The account segment (base64url, as the server spells it) of a `v2.`
 * access token, or `null` for anything else - a refresh credential, a
 * malformed string, an older format. Used to tell whether a bearer a
 * client captured belongs to the account the provider manages, without
 * verifying the tag (the client cannot, and does not need to).
 */
public fun accountSegmentOfAccessToken(token: String): String? {
    if (!token.startsWith(ACCESS_TOKEN_PREFIX)) return null
    val parts = token.split('.')
    if (parts.size != ACCESS_TOKEN_SEGMENTS) return null
    return parts[1].takeIf { it.isNotEmpty() }
}

/** `v2.<account>.<generation>.<expiry>.<tag>`. */
private const val ACCESS_TOKEN_SEGMENTS: Int = 5

private const val HTTP_UNAUTHORIZED_STATUS: Int = 401
