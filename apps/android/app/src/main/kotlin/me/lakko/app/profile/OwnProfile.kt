// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import android.content.Context
import kotlinx.coroutines.flow.update
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
    /**
     * When [displayName] was last set or cleared by the person (on this device
     * or, adopted with its stamp as is, on another of the account's devices),
     * UNIX milliseconds; 0 when no name was ever saved.
     *
     * Separate from [updatedAtMs] because that one also moves when the
     * session's username is stated ([OwnProfileStore.ensureUsername]): a fresh
     * device stamps its first profile at sign-in, which would otherwise read
     * as newer than a name saved earlier on another device and refuse it
     * (ADR-0048 section 4a). This is the stamp the own name travels between
     * the account's devices with.
     */
    val nameUpdatedAtMs: Long = if (displayName != null) updatedAtMs else 0L,
    /**
     * ADR-0084 — the profile photo (and video) this account publishes, or
     * `null` for none. Travels with the name: a change moves
     * [nameUpdatedAtMs] too, so the account's other devices adopt the name
     * and the photo as one.
     */
    val avatar: AvatarRefs? = null,
    /**
     * ADR-0092 section 7 — whether others may share this person's contact
     * card (profile v5 flags bit 0). `false` exactly while this account's
     * "who can find me" setting is `Nobody` ([cardShareableUnder]); kept here
     * rather than read at compose time so a change moves [updatedAtMs]
     * ([withCardShareable]): a receiver keeps only a strictly newer profile,
     * so a flag sent at the old stamp would never arrive.
     */
    val cardShareable: Boolean = true,
)

/** ADR-0092 section 7 — the card-sharing flag a profile carries under the
 *  "who can find me" [setting]: off exactly for `Nobody`. No setting (before
 *  a session binds) reads as shareable. */
public fun cardShareableUnder(setting: uniffi.lakko_ffi_uniffi.FfiVisibilitySetting?): Boolean =
    cardShareableForMode(setting?.mode)

/** [cardShareableUnder] by the setting's mode alone. */
public fun cardShareableForMode(mode: uniffi.lakko_ffi_uniffi.FfiVisibilityMode?): Boolean =
    mode != uniffi.lakko_ffi_uniffi.FfiVisibilityMode.NOBODY

/**
 * The own profile once it carries [shareable], or `null` when it already does
 * or none is stored (nothing to save or republish). The change moves
 * [OwnProfile.updatedAtMs] strictly forward so every receiver takes it; the
 * name's stamp stays (the name did not change).
 */
public fun withCardShareable(current: OwnProfile?, shareable: Boolean, nowMs: Long): OwnProfile? {
    if (current == null || current.cardShareable == shareable) return null
    return current.copy(
        cardShareable = shareable,
        updatedAtMs = nextProfileStamp(maxOf(current.updatedAtMs, current.nameUpdatedAtMs), nowMs),
    )
}

/**
 * The stamp for a profile changed at [nowMs] after one stamped [previousMs].
 *
 * Strictly greater than the previous stamp even when the wall clock went
 * backwards, because a receiver keeps only a STRICTLY newer profile — an
 * equal or older stamp would make this change invisible to every contact.
 */
/**
 * The own profile once it states the session's [username], or `null` when it
 * already does (nothing to save or republish).
 *
 * A device that holds no profile yet creates one at stamp 0, as the web and
 * iOS clients do for a name never saved here: it knows nothing newer than any
 * other device of the account, and a fresh stamp on a nameless profile would
 * outrank the name that device set when this one publishes into the account's
 * groups (every member keeps the strictly newest profile per account). Only a
 * username CHANGE of a record that exists moves the stamp.
 */
public fun withSessionUsername(current: OwnProfile?, username: String?, nowMs: Long): OwnProfile? {
    val canonical = username?.takeIf { it.isNotBlank() }
    if (current != null && current.username == canonical) return null
    if (current == null) {
        return OwnProfile(displayName = null, username = canonical, updatedAtMs = 0L, nameUpdatedAtMs = 0L)
    }
    return current.copy(
        username = canonical,
        updatedAtMs = nextProfileStamp(current.updatedAtMs, nowMs),
        // The name did not change: its stamp stays (ADR-0048 section 4a).
    )
}

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
): OwnProfile? = applyOwnProfileChange(current, newName, current?.avatar, username, nowMs)

/**
 * ADR-0084 — apply a name and photo change at once: `null` when nothing
 * changed, else the new profile with a fresh stamp. A new or removed photo
 * is a change like a new name (a strictly newer stamp, the republish path).
 */
public fun applyOwnProfileChange(
    current: OwnProfile?,
    newName: String?,
    newAvatar: AvatarRefs?,
    username: String?,
    nowMs: Long,
): OwnProfile? {
    if (current != null && current.displayName == newName && current.username == username &&
        current.avatar == newAvatar
    ) {
        return null
    }
    val stamp = nextProfileStamp(current?.let { maxOf(it.updatedAtMs, it.nameUpdatedAtMs) }, nowMs)
    val contentChanged = current == null || current.displayName != newName || current.avatar != newAvatar
    return OwnProfile(
        displayName = newName,
        username = username,
        updatedAtMs = stamp,
        nameUpdatedAtMs = if (contentChanged) stamp else current?.nameUpdatedAtMs ?: 0L,
        avatar = newAvatar,
        cardShareable = current?.cardShareable ?: true,
    )
}


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
        avatar = avatar?.toFfi(),
        cardShareable = cardShareable,
    )
}

/** Length of a username certificate (`usernameCertificateLen()`), spelled for
 *  the host tests. */
public const val USERNAME_CERTIFICATE_BYTES: Int = 153

/** Per-account persistence for [OwnProfile], keyed by the raw account id. */
public object OwnProfileStore {
    private const val PREFS = "lakko-own-profile"
    private val json = Json { ignoreUnknownKeys = true }

    private val mutableRevision = kotlinx.coroutines.flow.MutableStateFlow(0L)

    /** Moves on every [save] and [delete], so a screen showing the own name
     *  re-reads it when another of the account's devices changed it. */
    public val revision: kotlinx.coroutines.flow.StateFlow<Long> = mutableRevision

    @Serializable
    private data class Stored(
        val displayName: String? = null,
        val username: String? = null,
        val updatedAtMs: Long = 0L,
        /** Absent in a profile written before ADR-0048 section 4a. */
        val nameUpdatedAtMs: Long? = null,
        /** ADR-0084 — sealed under the device's avatar key (the refs carry
         *  the AEAD keys); absent before the profile photo. */
        val avatarSealed: String? = null,
        /** ADR-0092 section 7; absent before it, read as shareable. */
        val cardShareable: Boolean? = null,
    )

    private fun key(accountId: ByteArray): String =
        "own.v1." + accountId.joinToString("") { "%02x".format(it.toInt() and 0xff) }

    /** The stored profile for [accountId], or `null`. */
    public fun load(context: Context, accountId: ByteArray): OwnProfile? {
        val raw = prefs(context).getString(key(accountId), null) ?: return null
        return try {
            val s = json.decodeFromString<Stored>(raw)
            OwnProfile(
                displayName = s.displayName,
                username = s.username,
                updatedAtMs = s.updatedAtMs,
                nameUpdatedAtMs = s.nameUpdatedAtMs ?: if (s.displayName != null) s.updatedAtMs else 0L,
                avatar = me.lakko.app.avatar.openAvatarRefs(me.lakko.app.avatar.KeystoreAvatarSeal, s.avatarSealed),
                cardShareable = s.cardShareable ?: true,
            )
        } catch (_: Exception) {
            null
        }
    }

    /** Persist [profile] for [accountId]. */
    public fun save(context: Context, accountId: ByteArray, profile: OwnProfile) {
        // Not for an account whose session has ended (forget device removes
        // the row; nothing writes it back).
        me.lakko.app.account.AccountStoreGate.write(me.lakko.app.auth.base64Encode(accountId)) {
            val stored = Stored(profile.displayName, profile.username, profile.updatedAtMs, profile.nameUpdatedAtMs,
                me.lakko.app.avatar.sealAvatarRefs(me.lakko.app.avatar.KeystoreAvatarSeal, profile.avatar),
                profile.cardShareable)
            prefs(context).edit().putString(key(accountId), json.encodeToString(stored)).apply()
            mutableRevision.update { it + 1 }
        }
    }

    /**
     * Apply [transform] to the stored profile of [accountId] as one
     * read-modify-write (ADR-0048 section 4a: a name from another of the
     * account's devices); a `null` result stores nothing. Returns whether
     * something was stored.
     */
    public fun update(
        context: Context,
        accountId: ByteArray,
        transform: (OwnProfile?) -> OwnProfile?,
    ): Boolean = synchronized(this) {
        val next = transform(load(context, accountId)) ?: return false
        save(context, accountId, next)
        true
    }

    /** Remove the profile for [accountId] (forget device). */
    public fun delete(context: Context, accountId: ByteArray) {
        prefs(context).edit().remove(key(accountId)).apply()
        mutableRevision.update { it + 1 }
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
    ): Pair<OwnProfile, Boolean> = synchronized(this) {
        // Under the store's lock, as [update]: a name adopted from another
        // device between this read and the write below would be lost.
        val current = load(context, accountId)
        val updated = withSessionUsername(current, username, nowMs)
            // Null only when a stored profile already states this username.
            ?: return checkNotNull(current) to false
        save(context, accountId, updated)
        return updated to true
    }

    /**
     * ADR-0092 section 7 — make the stored profile's card-sharing flag follow
     * the "who can find me" [setting] (a local change or one adopted from
     * another device). Returns whether it changed, so the caller republishes
     * the profile everywhere it goes.
     */
    public fun reconcileCardShareable(
        context: Context,
        accountId: ByteArray,
        setting: uniffi.lakko_ffi_uniffi.FfiVisibilitySetting,
        nowMs: Long = System.currentTimeMillis(),
    ): Boolean = update(context, accountId) { withCardShareable(it, cardShareableUnder(setting), nowMs) }

    private fun prefs(context: Context) =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
}
