// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json

/**
 * ADR-0048 / UX §4 — this account's own profile: the name the person set
 * (signup, Settings -> Profile, or the guest `/c/` join screen), the username
 * it states alongside, and the stamp receivers order it by.
 *
 * Local only: it leaves the device solely inside the end-to-end encrypted
 * carriers (the contact request's sealed envelope and in-group MLS
 * application messages). Never uploaded, never in a backup the server reads.
 */
public data class OwnProfile(
    /** The person's own name, or `null` for none (others then see `@username`). */
    val displayName: String?,
    /** The account's canonical username, or `null` (guest accounts). */
    val username: String?,
    /** When this profile was last changed, UNIX milliseconds. */
    val updatedAtMs: Long,
)

/**
 * The stamp for a profile changed at [nowMs] after one stamped [previousMs].
 *
 * Strictly greater than the previous stamp even when the wall clock went
 * backwards, because a receiver keeps only a STRICTLY newer profile — an
 * equal or older stamp would make this change invisible to every contact.
 */
public fun nextProfileStamp(previousMs: Long?, nowMs: Long): Long =
    if (previousMs == null) nowMs else maxOf(nowMs, previousMs + 1)

/**
 * Apply a name change: `null` when nothing changed (no republish needed),
 * else the new profile with a fresh stamp.
 */
public fun applyOwnNameChange(
    current: OwnProfile?,
    newName: String?,
    username: String?,
    nowMs: Long,
): OwnProfile? {
    if (current != null && current.displayName == newName && current.username == username) return null
    return OwnProfile(
        displayName = newName,
        username = username,
        updatedAtMs = nextProfileStamp(current?.updatedAtMs, nowMs),
    )
}

/**
 * The Settings -> Profile live preview of how others see this account
 * (UX §4): `Alice Petrova · @alice`, `@alice` with no name, the name alone for
 * a guest. A name the rule refuses previews as no name — it will not be sent.
 */
public fun ownProfilePreview(nameCheck: DisplayNameCheck, username: String?): String {
    val name = (nameCheck as? DisplayNameCheck.Valid)?.normalized
    val user = username?.takeIf { it.isNotBlank() }
    return when {
        name != null && user != null -> "$name · @$user"
        name != null -> name
        user != null -> "@$user"
        else -> "Your key fingerprint"
    }
}

/** Copy shown after saving a profile name (UX §4). */
public const val PROFILE_SAVED_COPY: String = "Your name will update for your contacts."

/**
 * The wire form of this profile for the core's composer (profile v3,
 * ADR-0060 §3).
 *
 * @param accountId this account's raw 16-byte auth account id (ADR-0048 v2):
 *   what a receiver derives the ADR-0021 call peer tag from, so a group member
 *   who is not a contact still gets a name on their call tile. Sent only when
 *   it is exactly 16 bytes — the composer refuses any other length, and a
 *   profile with no id is better than no profile at all.
 * @param grants the reach grants handed to the receivers (at most two, one
 *   per class). SECRET.
 * @param usernameCertificate the discovery-signed certificate of the username
 *   (153 bytes), which lets a receiver show the username without a lookup.
 * @param shareUsername whether the username goes out at all: only where the
 *   receivers may find this person (a one-to-one, a group the setting allows).
 *   Everywhere else the profile carries the name alone.
 */
public fun OwnProfile.toFfi(
    accountId: ByteArray?,
    grants: List<uniffi.lakko_ffi_uniffi.FfiReachGrant> = emptyList(),
    usernameCertificate: ByteArray? = null,
    shareUsername: Boolean = true,
): uniffi.lakko_ffi_uniffi.FfiProfile {
    val stated = username?.takeIf { it.isNotEmpty() && shareUsername }
    return uniffi.lakko_ffi_uniffi.FfiProfile(
        displayName = displayName,
        username = stated,
        updatedAtMs = updatedAtMs.coerceAtLeast(0L).toULong(),
        accountId = accountId?.takeIf { it.size == ACCOUNT_ID_BYTES },
        usernameCertificate = usernameCertificate?.takeIf { stated != null && it.size == USERNAME_CERTIFICATE_BYTES },
        grants = grants,
    )
}

/** Length of a username certificate (`usernameCertificateLen()`), spelled for
 *  the host tests. */
public const val USERNAME_CERTIFICATE_BYTES: Int = 153

/** Per-account persistence for [OwnProfile], keyed by the raw account id. */
public object OwnProfileStore {
    private const val PREFS = "lakko-own-profile"
    private val json = Json { ignoreUnknownKeys = true }

    @Serializable
    private data class Stored(
        val displayName: String? = null,
        val username: String? = null,
        val updatedAtMs: Long = 0L,
    )

    private fun key(accountId: ByteArray): String =
        "own.v1." + accountId.joinToString("") { "%02x".format(it.toInt() and 0xff) }

    /** The stored profile for [accountId], or `null`. */
    public fun load(context: Context, accountId: ByteArray): OwnProfile? {
        val raw = prefs(context).getString(key(accountId), null) ?: return null
        return try {
            val s = json.decodeFromString<Stored>(raw)
            OwnProfile(s.displayName, s.username, s.updatedAtMs)
        } catch (_: Exception) {
            null
        }
    }

    /** Persist [profile] for [accountId]. */
    public fun save(context: Context, accountId: ByteArray, profile: OwnProfile) {
        val stored = Stored(profile.displayName, profile.username, profile.updatedAtMs)
        prefs(context).edit().putString(key(accountId), json.encodeToString(stored)).apply()
    }

    /** Remove the profile for [accountId] (forget device). */
    public fun delete(context: Context, accountId: ByteArray) {
        prefs(context).edit().remove(key(accountId)).apply()
    }

    /**
     * Make sure a profile exists for [accountId] and states [username]
     * (normalised, or `null` for a guest). Returns the profile and whether it
     * changed, so the caller republishes only on a real change.
     */
    public fun ensureUsername(
        context: Context,
        accountId: ByteArray,
        username: String?,
        nowMs: Long = System.currentTimeMillis(),
    ): Pair<OwnProfile, Boolean> {
        val canonical = username?.takeIf { it.isNotBlank() }
        val current = load(context, accountId)
        if (current != null && current.username == canonical) return current to false
        val updated = OwnProfile(
            displayName = current?.displayName,
            username = canonical,
            updatedAtMs = nextProfileStamp(current?.updatedAtMs, nowMs),
        )
        save(context, accountId, updated)
        return updated to true
    }

    private fun prefs(context: Context) =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
