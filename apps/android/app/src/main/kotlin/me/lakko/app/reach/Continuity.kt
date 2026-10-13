// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/*
 * ADR-0073 "Phase 2 revised", R.4 - the CONTINUITY reach key.
 *
 * A reset changes every reach key and the mailbox pseudonym, and the new
 * identity knows none of its former contacts, so nobody holds a grant for its
 * new mailbox. The continuity key is what lets a peer that ACCEPTED the reset
 * deliver a Welcome there:
 *
 *     continuity seed = continuityReachSeed(account id, key generation)
 *
 * Both sides derive it from public values the peer verified:
 *
 *   * the RESETTING device registers the key's public half on its new account
 *     mailbox in its reset order, with an expiry (this file's marker; the
 *     reach manager keeps it registered until then);
 *   * a PEER signs its Welcome with `DsProofSigner.forReach(seed)`.
 *
 * It is a write permission, not a secret that protects content. Mirror of
 * `apps/web/src/reach/continuity.ts`.
 */

/** How long the continuity key stays registered, from the reset's
 *  `signed_at`: the DS journal retention (decision D2). Chosen for parity
 *  with how long an offline peer's traffic survives anyway, not measured. */
public const val CONTINUITY_KEY_TTL_MS: Long = 30L * 24 * 60 * 60 * 1000

/** The expiry the key is registered with. */
public fun continuityExpiryMs(signedAtMs: Long): Long = signedAtMs + CONTINUITY_KEY_TTL_MS

/** What the resetting device remembers about its own last reset. */
@Serializable
public data class ContinuityMarker(
    /** The generation the reset moved the account to. */
    val keyGeneration: Long,
    /** The attestation's `signed_at`, UNIX milliseconds. */
    val signedAtMs: Long,
)

private val markerJson = Json { ignoreUnknownKeys = true }

/** The marker as stored. */
public fun encodeContinuityMarker(marker: ContinuityMarker): String =
    markerJson.encodeToString(ContinuityMarker.serializer(), marker)

/** The marker, or `null` (none, or an unreadable row). */
public fun decodeContinuityMarker(raw: String?): ContinuityMarker? {
    if (raw.isNullOrEmpty()) return null
    return try {
        markerJson.decodeFromString(ContinuityMarker.serializer(), raw).takeIf { it.keyGeneration > 0 }
    } catch (_: Exception) {
        null
    }
}

/** The key the resetting device wants registered now. */
public data class WantedContinuityKey(val keyGeneration: Long, val expiresAtMs: Long)

/**
 * Whether the continuity key is still to be kept registered:
 *
 * | marker                         | wanted                         |
 * |--------------------------------|--------------------------------|
 * | none                           | no                             |
 * | present, before the expiry     | yes, with that expiry          |
 * | present, at or past the expiry | no (the reach sync removes it) |
 */
public fun continuityKeyWanted(marker: ContinuityMarker?, nowMs: Long): WantedContinuityKey? {
    if (marker == null) return null
    val expiresAtMs = continuityExpiryMs(marker.signedAtMs)
    if (nowMs >= expiresAtMs) return null
    return WantedContinuityKey(marker.keyGeneration, expiresAtMs)
}

/**
 * Whether a PEER still signs a Welcome with the continuity key: inside the
 * same window, counted from the attestation's `signed_at` it verified. Past
 * it the key is no longer registered and the write would be refused.
 */
public fun continuityKeyUsable(signedAtMs: Long, nowMs: Long): Boolean = nowMs < continuityExpiryMs(signedAtMs)

/** A continuity key as the mailbox holds it: its public key (hex) and the
 *  expiry it was registered with. */
@Serializable
public data class RegisteredContinuityKey(val publicKeyHex: String, val expiresAtMs: Long)

/** What one reach sync changes about the continuity key. */
public data class ContinuityChange(
    /** The key to register (with its expiry), or `null`. */
    val add: RegisteredContinuityKey?,
    /** The public key (hex) to unregister, or `null`. */
    val removeHex: String?,
) {
    public val isEmpty: Boolean get() = add == null && removeHex == null
}

/**
 * What to send so the mailbox holds exactly the wanted continuity key.
 *
 * | wanted                | registered                    | change                          |
 * |-----------------------|-------------------------------|---------------------------------|
 * | none                  | none                          | nothing                         |
 * | none (expired / gone) | a key                         | remove it                       |
 * | a key                 | none                          | add it                          |
 * | a key                 | the same key, the same expiry | nothing                         |
 * | a key                 | the same key, another expiry  | add it (the expiry is replaced) |
 * | a key                 | another key (a later reset)   | add the new one, remove the old |
 */
public fun continuityChange(wanted: RegisteredContinuityKey?, registered: RegisteredContinuityKey?): ContinuityChange =
    when {
        wanted == null -> ContinuityChange(null, registered?.publicKeyHex)
        registered == null -> ContinuityChange(wanted, null)
        registered == wanted -> ContinuityChange(null, null)
        registered.publicKeyHex == wanted.publicKeyHex -> ContinuityChange(wanted, null)
        else -> ContinuityChange(wanted, registered.publicKeyHex)
    }

/**
 * The marker of this device's own last reset, per account: plain
 * preferences, public values only (a generation and a time). Written in the
 * reset's client order; removed with the account's data. Never throws: with
 * storage blocked the key is not registered and former contacts reach the
 * account only through an open mailbox or a new contact request.
 */
public object ContinuityMarkerStore {
    private const val PREFS = "lakko-reach-continuity"
    private const val KEY_PREFIX = "marker-v1:"

    public fun write(context: android.content.Context, accountIdB64: String, marker: ContinuityMarker) {
        try {
            context.applicationContext.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE)
                .edit().putString(KEY_PREFIX + accountIdB64, encodeContinuityMarker(marker)).apply()
        } catch (_: Throwable) {
            // storage blocked
        }
    }

    public fun read(context: android.content.Context, accountIdB64: String): ContinuityMarker? =
        try {
            decodeContinuityMarker(
                context.applicationContext.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE)
                    .getString(KEY_PREFIX + accountIdB64, null),
            )
        } catch (_: Throwable) {
            null
        }

    public fun clear(context: android.content.Context, accountIdB64: String) {
        try {
            context.applicationContext.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE)
                .edit().remove(KEY_PREFIX + accountIdB64).apply()
        } catch (_: Throwable) {
            // nothing to clear
        }
    }
}
