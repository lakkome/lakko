// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import me.lakko.app.l10n.UiText
import me.lakko.app.push.GroupPushRouting

/**
 * ADR-0051 — the shared group name on the Android client: the stamped
 * per-`(account, group)` store, the receive-side verdict, and the wire peek.
 *
 * ## What a name is
 *
 * A name is a fact one person asserts about the whole group, carried as a
 * bounded envelope on an MLS application message (`lakko/group-name/v1`). The
 * sender's authority is the RECEIVER's check (ADR-0051 §2): the MLS sender leaf
 * is resolved to the account key at receipt (C-68), the LIVE role table is
 * read, and only the owner or an admin may set the name — any member when the
 * group carries no role table. Then the stamp decides: strictly newer wins,
 * equal is not newer, so a growth re-send of the stored name is not an update
 * (the ADR-0044 lesson: the stamp is persisted and re-emitted verbatim).
 *
 * ## Why the verdict is a pure function
 *
 * The push-routing gate ([GroupPushRouting.decideAcceptReceivedSecret]) is the
 * template, and for the same reason: the decision must be readable and
 * testable on the JVM without the UniFFI `.so`, and it must run against
 * CURRENT state, under the per-group lock, in the poll's own critical section
 * (C-62 sub-invariant 5). The repository reads the role table and the stored
 * row, calls this, and writes back — nothing in between.
 */

/** A group's stored shared name. An empty [name] means "cleared" — the title
 *  falls back to the member-derived one; the stamp still orders later sets. */
public data class StoredGroupName(
    val name: String,
    val updatedAtMs: ULong,
)

/** The receive-side decision for an incoming group name. */
public enum class GroupNameVerdict(public val accepts: Boolean) {
    /** The same name at the same stamp as the stored one — a growth re-send or
     *  a relay. Idempotent; needs no authority and no ordering. */
    ACCEPT_IDENTICAL(true),

    /** A newer name from the ADR-0022 role-table owner. */
    ACCEPT_OWNER(true),

    /** A newer name from an ADR-0022 admin. */
    ACCEPT_ADMIN(true),

    /** A newer name in a group with no role table (legacy / open group). */
    ACCEPT_NO_ROLE_TABLE(true),

    /** A changed name from a plain member (or a key not in the roster). */
    REJECT_NOT_AUTHORIZED(false),

    /** The role table could not be read, or the sender could not be resolved
     *  in a role-table group. Fail closed. */
    REJECT_UNVERIFIABLE(false),

    /** Authorised, but not newer than what is stored. */
    REJECT_STALE(false),
}

/**
 * Decide whether [incoming] replaces [stored] for one group.
 *
 * @param senderSignatureKey the sender's account key resolved from the MLS
 *   leaf at receipt, raw bytes — `null` when unresolvable.
 * @param owner the live role table (owner + admins), no table, or unreadable.
 * @param isNewer the canonical order — the FFI `groupNameIsNewer`, threaded in
 *   so the comparison is defined once in Rust.
 */
public fun groupNameVerdict(
    senderSignatureKey: ByteArray?,
    owner: GroupPushRouting.OwnerLookup,
    incoming: StoredGroupName,
    stored: StoredGroupName?,
    isNewer: (incomingMs: ULong, storedMs: ULong) -> Boolean,
): GroupNameVerdict {
    // (1) Identical name AND stamp: a re-send of what is held. Inert for every
    // sender, so it needs neither authority nor ordering.
    if (stored != null && stored.name == incoming.name && stored.updatedAtMs == incoming.updatedAtMs) {
        return GroupNameVerdict.ACCEPT_IDENTICAL
    }
    // (2) Authority — who may set a name.
    val authorised = when (owner) {
        is GroupPushRouting.OwnerLookup.Unavailable -> GroupNameVerdict.REJECT_UNVERIFIABLE
        is GroupPushRouting.OwnerLookup.NoRoleTable -> GroupNameVerdict.ACCEPT_NO_ROLE_TABLE
        is GroupPushRouting.OwnerLookup.Known -> when {
            senderSignatureKey == null -> GroupNameVerdict.REJECT_UNVERIFIABLE
            senderSignatureKey.contentEquals(owner.owner) -> GroupNameVerdict.ACCEPT_OWNER
            owner.admins.any { it.contentEquals(senderSignatureKey) } -> GroupNameVerdict.ACCEPT_ADMIN
            else -> GroupNameVerdict.REJECT_NOT_AUTHORIZED
        }
    }
    if (!authorised.accepts) return authorised
    // (3) Order — which authorised set survives. Nothing stored: the first name.
    if (stored == null) return authorised
    return if (isNewer(incoming.updatedAtMs, stored.updatedAtMs)) authorised else GroupNameVerdict.REJECT_STALE
}

/**
 * ADR-0051 — the frozen group-name envelope domain separator,
 * `lakko/group-name/v1` followed by one NUL byte (20 bytes). Built
 * programmatically: a raw NUL in a source file makes git treat it as binary.
 */
internal val GROUP_NAME_ENVELOPE_DOMAIN: ByteArray =
    "lakko/group-name/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

/**
 * Does this application plaintext open with the group-name domain separator?
 *
 * As with the profile peek this is a complete CLASSIFICATION: the separator
 * ends in a NUL byte, so no chat envelope (`{`) and no legacy UTF-8 message a
 * person typed can open with it. A plaintext that does is a group-name
 * envelope whether or not it decodes; one that does not decode is DROPPED, not
 * handed to the chat parser whose lossy-UTF-8 fallback would render the domain
 * string as a bubble. `internal` so the background push-preview filter reuses
 * this exact test. Pure — host-testable.
 */
internal fun looksLikeGroupNameEnvelope(bytes: ByteArray): Boolean {
    if (bytes.size < GROUP_NAME_ENVELOPE_DOMAIN.size) return false
    for (i in GROUP_NAME_ENVELOPE_DOMAIN.indices) {
        if (bytes[i] != GROUP_NAME_ENVELOPE_DOMAIN[i]) return false
    }
    return true
}

private val groupNameJson = Json { ignoreUnknownKeys = true }

/** The persisted row shape; the stamp as text so a full `u64` survives. */
@Serializable
private data class StoredGroupNameRow(
    val name: String = "",
    val updatedAtMs: String = "",
)

/** The store row as JSON `{ "name": ..., "updatedAtMs": "<u64 as text>" }`. */
internal fun encodeStoredGroupName(value: StoredGroupName): String =
    groupNameJson.encodeToString(StoredGroupNameRow(name = value.name, updatedAtMs = value.updatedAtMs.toString()))

/** The inverse of [encodeStoredGroupName]; `null` for anything malformed. */
internal fun decodeStoredGroupName(raw: String?): StoredGroupName? {
    if (raw.isNullOrEmpty()) return null
    return try {
        val row = groupNameJson.decodeFromString<StoredGroupNameRow>(raw)
        val stamp = row.updatedAtMs.toULongOrNull() ?: return null
        StoredGroupName(name = row.name, updatedAtMs = stamp)
    } catch (_: Exception) {
        null
    }
}

/**
 * The per-`(account, group)` shared-name store: plain `SharedPreferences` (a
 * name is content the group already shares, not key material). [version]
 * bumps on every write so a title computed off the store recomposes.
 */
public object GroupNameStore {
    private const val PREFS = "lakko-group-names"
    private const val KEY_PREFIX = "lakko.group-name.v1:"

    private val mutableVersion = MutableStateFlow(0L)

    /** Bumped after every write; observe it to re-read titles. */
    public val version: StateFlow<Long> = mutableVersion.asStateFlow()

    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun key(accountIdB64: String, groupIdHex: String): String =
        KEY_PREFIX + accountIdB64 + ":" + groupIdHex.lowercase()

    /** The stored row, or `null` when this device holds no name for the group. */
    public fun load(context: Context, accountIdB64: String?, groupIdHex: String): StoredGroupName? {
        if (accountIdB64 == null) return null
        return decodeStoredGroupName(prefs(context).getString(key(accountIdB64, groupIdHex), null))
    }

    /** The title-worthy name: the stored name when it is not blank. */
    public fun name(context: Context, accountIdB64: String?, groupIdHex: String): String? =
        load(context, accountIdB64, groupIdHex)?.name?.takeIf { it.isNotBlank() }

    public fun save(context: Context, accountIdB64: String, groupIdHex: String, value: StoredGroupName) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            prefs(context).edit().putString(key(accountIdB64, groupIdHex), encodeStoredGroupName(value)).apply()
            mutableVersion.value += 1
        }
    }

    /** Forget every group name held for [accountIdB64] (forget device). */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        val p = prefs(context)
        val prefix = KEY_PREFIX + accountIdB64 + ":"
        val e = p.edit()
        for (k in p.all.keys) if (k.startsWith(prefix)) e.remove(k)
        e.apply()
        mutableVersion.value += 1
    }
}

/** UX copy (docs/ux/group-and-channel-names.md section 5). */
public val GROUP_NAME_LABEL: UiText = UiText("chat.group_name.label")
public val GROUP_NAME_SAVE: UiText = UiText("common.save")
public val GROUP_NAME_ONLY_ADMINS: UiText = UiText("chat.group_name.only_admins")
public val GROUP_NAME_SEND_FAILED: UiText = UiText("chat.group_name.error.save")
