// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.auth

/**
 * ADR-0073 section 3 — the session pair every session-landing response
 * carries: a short-lived access token (the bearer of every service), the
 * refresh credential only `POST /auth/session/refresh` accepts, and the key
 * generation both were issued under. Treated as opaque strings; the
 * generation is what tells a device that its keys are stale.
 *
 * Never printed: [toString] redacts both credentials.
 */
public class SessionPair(
    /** `v3.<account>.<generation>.<device>.<issued>.<expiry>.<tag>` - the
     *  bearer (ADR-0074: it names the device it was issued to). */
    public val accessToken: String,
    /** Unix milliseconds; the access token is unusable at or after this. */
    public val accessExpiresAtMs: Long,
    /** `r2.<account>.<generation>.<device>.<issued>.<expiry>.<tag>` -
     *  refresh only. */
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

/** ADR-0074 section 2 - the `error` code auth answers (`401`, on any bearer
 *  route and on the refresh route) to a device another device of the account
 *  removed. Discovery answers the same code with a `403` on a KeyPackage
 *  publish; that one is NOT proof of removal (see [deviceRevokedHintOf]). */
public const val DEVICE_REVOKED: String = "device-revoked"

/**
 * What a `401` means for this device (ADR-0073 section 3.3 and 3.5; the
 * contract's section 7). Pure over the status and the body's `error` code.
 */
public enum class SessionRefusal {
    /** `key-generation-superseded`: the account's keys were reset on another
     *  device. This device signs out and wipes; nothing is retried. */
    KeysReset,

    /** `device-revoked` (ADR-0074): another device of the account removed
     *  this one. This device shows the removed screen and wipes everything
     *  by itself; nothing is retried, and no recall record is kept. */
    DeviceRevoked,

    /** `refresh-invalid`: the refresh credential is gone (expired, unknown,
     *  or this device logged out at the server). The device goes to the
     *  sign-in screen; nothing is wiped. */
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
        DEVICE_REVOKED -> SessionRefusal.DeviceRevoked
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

/**
 * Whether a response is discovery's `403 device-revoked` (a KeyPackage
 * publish under a tombstoned device id). It is a HINT, never proof of
 * removal (ADR-0074 "T1 as built" point 24): the client refreshes at auth
 * and acts on auth's answer, which is `401 device-revoked` when the device
 * really was removed and a new pair when it was not.
 */
public fun deviceRevokedHintOf(status: Int, errorCode: String?): Boolean =
    status == HTTP_FORBIDDEN_STATUS && errorCode == DEVICE_REVOKED

/** The version prefix of an access token (ADR-0074 flag day: `v2` is gone). */
public const val ACCESS_TOKEN_PREFIX: String = "v3."

/** The version prefix of a refresh credential. */
public const val REFRESH_TOKEN_PREFIX: String = "r2."

/**
 * The account segment (base64url, as the server spells it) of a `v3.`
 * access token, or `null` for anything else - a refresh credential, a
 * fresh-auth grant, a malformed string, an older format. Used to tell
 * whether a bearer a client captured belongs to the account the provider
 * manages, without verifying the tag (the client cannot, and does not need
 * to).
 */
public fun accountSegmentOfAccessToken(token: String): String? =
    accessTokenSegments(token)?.get(ACCOUNT_SEGMENT_INDEX)?.takeIf { it.isNotEmpty() }

/**
 * The device segment (base64url of 32 bytes) of a `v3.` access token, or
 * `null` for anything that is not one. [UNBOUND_DEVICE_SEGMENT] means the
 * session names no device yet. Read only to decide whether this device still
 * has to bind itself ([deviceBindingActionFor]); the tag is not verified.
 */
public fun deviceSegmentOfAccessToken(token: String): String? =
    accessTokenSegments(token)?.get(DEVICE_SEGMENT_INDEX)?.takeIf { it.isNotEmpty() }

/** The all-zero device id as a token spells it: "no device identity yet". */
public val UNBOUND_DEVICE_SEGMENT: String = "A".repeat(43)

private fun accessTokenSegments(token: String): List<String>? {
    if (!token.startsWith(ACCESS_TOKEN_PREFIX)) return null
    val parts = token.split('.')
    return parts.takeIf { it.size == ACCESS_TOKEN_SEGMENTS }
}

/** `v3.<account>.<generation>.<device>.<issued>.<expiry>.<tag>`. */
private const val ACCESS_TOKEN_SEGMENTS: Int = 7
private const val ACCOUNT_SEGMENT_INDEX: Int = 1
private const val DEVICE_SEGMENT_INDEX: Int = 3

private const val HTTP_UNAUTHORIZED_STATUS: Int = 401
private const val HTTP_FORBIDDEN_STATUS: Int = 403
