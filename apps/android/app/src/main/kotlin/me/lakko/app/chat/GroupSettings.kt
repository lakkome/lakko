// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import me.lakko.app.profile.AvatarRefs
import me.lakko.app.push.GroupPushRouting

/*
 * ADR-0087 sections 5, 11 and 18.1, reactions-and-emoji section 9 — the group
 * settings that ride a group's journal as service envelopes: the group photo,
 * the allowed reactions, the conversation handle (the group's @name and
 * public / private flag) and the knock seed the owner hands to an admin.
 *
 * Each domain ends in a NUL byte, so a prefix match is a complete
 * classification: such a plaintext is never handed to the chat parser, whose
 * lossy-UTF-8 fallback would draw the domain as a bubble.
 *
 * The photo and the reactions follow the group-name rule (ADR-0051): the owner
 * or an admin resolved at receipt, under the per-group lock, strictly newer
 * stamp; a growth re-send carries the stored stamp. The handle needs no role
 * check: its signature by the handle key proves it, so any member relays it.
 */

/** The four group-setting envelopes. */
public enum class GroupSettingKind(domain: String) {
    /** `lakko/group-photo/v1` — the group's photo (owner / admin). */
    PHOTO("lakko/group-photo/v1"),

    /** `lakko/group-reactions/v1` — the allowed reactions (owner / admin). */
    REACTIONS("lakko/group-reactions/v1"),

    /** `lakko/conversation-handle/v1` — the @name and flag (signed). */
    HANDLE("lakko/conversation-handle/v1"),

    /** `lakko/group-knock-seed/v1` — the knock seed for one admin (owner). */
    KNOCK_SEED("lakko/group-knock-seed/v1"),
    ;

    /** The frozen domain separator: the ASCII name followed by one NUL. */
    public val prefix: ByteArray = domain.toByteArray(Charsets.US_ASCII) + byteArrayOf(0)
}

/** The group-setting kind [bytes] opens with, or `null`. Pure. */
public fun groupSettingKindOf(bytes: ByteArray): GroupSettingKind? {
    for (kind in GroupSettingKind.entries) {
        val prefix = kind.prefix
        if (bytes.size < prefix.size) continue
        var match = true
        for (i in prefix.indices) {
            if (bytes[i] != prefix[i]) {
                match = false
                break
            }
        }
        if (match) return kind
    }
    return null
}

/** A group's allowed reactions, in a form the stores and the host tests can
 *  hold without the FFI. */
public sealed interface AllowedReactions {
    /** Any reaction (the default). */
    public data object All : AllowedReactions

    /** No reactions. */
    public data object Off : AllowedReactions

    /** Only these, in the setter's order. */
    public data class Only(val emoji: List<String>) : AllowedReactions
}

/** The FFI form. */
public fun AllowedReactions.toFfiAllowed(): uniffi.lakko_ffi_uniffi.FfiAllowedReactions = when (this) {
    AllowedReactions.All -> uniffi.lakko_ffi_uniffi.FfiAllowedReactions.All
    AllowedReactions.Off -> uniffi.lakko_ffi_uniffi.FfiAllowedReactions.Off
    is AllowedReactions.Only -> uniffi.lakko_ffi_uniffi.FfiAllowedReactions.Only(emoji)
}

/** From the FFI form. */
public fun allowedReactionsOf(value: uniffi.lakko_ffi_uniffi.FfiAllowedReactions): AllowedReactions = when (value) {
    uniffi.lakko_ffi_uniffi.FfiAllowedReactions.All -> AllowedReactions.All
    uniffi.lakko_ffi_uniffi.FfiAllowedReactions.Off -> AllowedReactions.Off
    is uniffi.lakko_ffi_uniffi.FfiAllowedReactions.Only -> AllowedReactions.Only(value.emoji)
}

/** A stored group photo. A `null` [avatar] means "removed"; the stamp still
 *  orders later sets. */
public data class StoredGroupPhoto(val avatar: AvatarRefs?, val updatedAtMs: ULong)

/** A stored allowed-reactions setting. */
public data class StoredGroupReactions(val reactions: AllowedReactions, val updatedAtMs: ULong)

/** A stored conversation handle, kept verbatim so a re-send relays the
 *  owner's signature unchanged. */
public data class StoredGroupHandle(
    val handle: String,
    val public: Boolean,
    val updatedAtMs: ULong,
    val certificate: ByteArray,
    val signature: ByteArray,
    /** ADR-0087 section 3 — the certificate was checked against discovery's
     *  pinned key and the lookup key of the shown name. Only a verified
     *  @name is shown (fail closed). Not part of the value's identity. */
    val verified: Boolean = false,
) {
    override fun equals(other: Any?): Boolean =
        other is StoredGroupHandle && handle == other.handle && public == other.public &&
            updatedAtMs == other.updatedAtMs && certificate.contentEquals(other.certificate) &&
            signature.contentEquals(other.signature)

    override fun hashCode(): Int = handle.hashCode() * 31 + updatedAtMs.hashCode()

    /** The FFI value, for a relay. */
    public fun toFfi(): uniffi.lakko_ffi_uniffi.FfiConversationHandle =
        uniffi.lakko_ffi_uniffi.FfiConversationHandle(
            updatedAtMs = updatedAtMs,
            public = public,
            handle = handle,
            certificate = certificate,
            signature = signature,
        )
}

/**
 * The owner-or-admin receive verdict, shared by the photo and the reactions
 * (the group-name rule, ADR-0051 section 2). [identical] says the incoming
 * value equals the stored one at the same stamp (a growth re-send); [newer]
 * is the core's strictly-greater order, consulted only when something is
 * stored. Pure — host-testable.
 */
public fun ownerOrAdminVerdict(
    senderKey: ByteArray?,
    owner: GroupPushRouting.OwnerLookup,
    hasStored: Boolean,
    identical: Boolean,
    newer: () -> Boolean,
): GroupNameVerdict {
    if (hasStored && identical) return GroupNameVerdict.ACCEPT_IDENTICAL
    val authorised = when (owner) {
        is GroupPushRouting.OwnerLookup.Unavailable -> GroupNameVerdict.REJECT_UNVERIFIABLE
        is GroupPushRouting.OwnerLookup.NoRoleTable -> GroupNameVerdict.ACCEPT_NO_ROLE_TABLE
        is GroupPushRouting.OwnerLookup.Known -> when {
            senderKey == null -> GroupNameVerdict.REJECT_UNVERIFIABLE
            senderKey.contentEquals(owner.owner) -> GroupNameVerdict.ACCEPT_OWNER
            owner.admins.any { it.contentEquals(senderKey) } -> GroupNameVerdict.ACCEPT_ADMIN
            else -> GroupNameVerdict.REJECT_NOT_AUTHORIZED
        }
    }
    if (!authorised.accepts) return authorised
    if (!hasStored) return authorised
    return if (newer()) authorised else GroupNameVerdict.REJECT_STALE
}

private val settingsJson = Json { ignoreUnknownKeys = true }

@Serializable
private data class PhotoRow(val avatar: AvatarRefs? = null, val updatedAtMs: String = "")

@Serializable
private data class ReactionsRow(val mode: String = "all", val emoji: List<String> = emptyList(), val updatedAtMs: String = "")

@Serializable
private data class HandleRow(
    val handle: String = "",
    val public: Boolean = false,
    val updatedAtMs: String = "",
    val certificateB64: String = "",
    val signatureB64: String = "",
    val verified: Boolean = false,
)

internal fun encodeGroupPhoto(value: StoredGroupPhoto): String =
    settingsJson.encodeToString(PhotoRow(avatar = value.avatar, updatedAtMs = value.updatedAtMs.toString()))

internal fun decodeGroupPhoto(raw: String?): StoredGroupPhoto? {
    if (raw.isNullOrEmpty()) return null
    return try {
        val row = settingsJson.decodeFromString<PhotoRow>(raw)
        StoredGroupPhoto(row.avatar, row.updatedAtMs.toULongOrNull() ?: return null)
    } catch (_: Exception) {
        null
    }
}

internal fun encodeGroupReactions(value: StoredGroupReactions): String {
    val (mode, emoji) = when (val r = value.reactions) {
        AllowedReactions.All -> "all" to emptyList()
        AllowedReactions.Off -> "off" to emptyList()
        is AllowedReactions.Only -> "only" to r.emoji
    }
    return settingsJson.encodeToString(ReactionsRow(mode, emoji, value.updatedAtMs.toString()))
}

internal fun decodeGroupReactions(raw: String?): StoredGroupReactions? {
    if (raw.isNullOrEmpty()) return null
    return try {
        val row = settingsJson.decodeFromString<ReactionsRow>(raw)
        val stamp = row.updatedAtMs.toULongOrNull() ?: return null
        val reactions = when (row.mode) {
            "all" -> AllowedReactions.All
            "off" -> AllowedReactions.Off
            "only" -> if (row.emoji.isEmpty()) return null else AllowedReactions.Only(row.emoji)
            else -> return null
        }
        StoredGroupReactions(reactions, stamp)
    } catch (_: Exception) {
        null
    }
}

private val b64e = java.util.Base64.getEncoder()
private val b64d = java.util.Base64.getDecoder()

internal fun encodeGroupHandle(value: StoredGroupHandle): String =
    settingsJson.encodeToString(
        HandleRow(
            handle = value.handle,
            public = value.public,
            updatedAtMs = value.updatedAtMs.toString(),
            certificateB64 = b64e.encodeToString(value.certificate),
            signatureB64 = b64e.encodeToString(value.signature),
            verified = value.verified,
        ),
    )

internal fun decodeGroupHandle(raw: String?): StoredGroupHandle? {
    if (raw.isNullOrEmpty()) return null
    return try {
        val row = settingsJson.decodeFromString<HandleRow>(raw)
        StoredGroupHandle(
            handle = row.handle,
            public = row.public,
            updatedAtMs = row.updatedAtMs.toULongOrNull() ?: return null,
            certificate = b64d.decode(row.certificateB64),
            signature = b64d.decode(row.signatureB64),
            verified = row.verified,
        )
    } catch (_: Exception) {
        null
    }
}

/**
 * The per-`(account, group)` store of the group settings, plus the admin's
 * knock seeds and the uploader's owed deletions of a replaced group photo.
 * Encrypted preferences: a photo carries its blob key and a knock seed opens
 * every request to join, both secret to the members. [version] bumps on every
 * write so a screen drawn off the store recomposes.
 */
public object GroupSettingsStore {
    private const val PREFS = "lakko-group-settings"
    private const val PHOTO = "photo:"
    private const val REACTIONS = "reactions:"
    private const val HANDLE = "handle:"
    private const val KNOCK = "knock:"
    private const val OWED_DELETE = "owed-delete:"
    private const val LEASE = "lease:"
    private const val OWED_PHOTO = "owed-photo:"
    private const val OWED_REKEY = "owed-rekey:"
    private const val CREATED = "created:"
    private const val KNOCK_SELF_OWED = "knock-self-owed:"
    private const val KNOCK_BOUNDARY = "knock-boundary:"
    private const val OWED_TYPE = "owed-type:"
    private const val KNOCK_VERSION = "knock-version:"
    private const val KNOCK_ADMINS = "knock-admins:"
    private const val KNOCK_ORIGIN = "knock-origin:"
    private const val OWED_CHANNEL_RECORD = "owed-channel-record:"
    private const val KNOCK_CURSOR = "knock-cursor:"
    private const val KNOCK_REPLY = "knock-reply:"
    private const val CURSOR_EPOCH = "cursor-epoch:"

    private val mutableVersion = MutableStateFlow(0L)

    /** Bumped after every write. */
    public val version: StateFlow<Long> = mutableVersion.asStateFlow()

    @Volatile
    private var cached: SharedPreferences? = null

    private fun prefs(context: Context): SharedPreferences {
        cached?.let { return it }
        return synchronized(this) {
            cached ?: run {
                val app = context.applicationContext
                val master = MasterKey.Builder(app).setKeyScheme(MasterKey.KeyScheme.AES256_GCM).build()
                EncryptedSharedPreferences.create(
                    app,
                    PREFS,
                    master,
                    EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                    EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
                ).also { cached = it }
            }
        }
    }

    private fun key(prefix: String, accountIdB64: String, groupIdHex: String): String =
        prefix + accountIdB64 + ":" + groupIdHex.lowercase()

    private fun read(context: Context, k: String): String? =
        try {
            prefs(context).getString(k, null)
        } catch (_: Throwable) {
            null
        }

    private fun write(context: Context, accountIdB64: String, k: String, value: String?) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val e = prefs(context).edit()
            if (value == null) e.remove(k) else e.putString(k, value)
            e.apply()
            mutableVersion.value += 1
        }
    }

    /** Section 5 — how far this device has handled the group's knock queue,
     *  saved only once every request read up to it (and its reply grant) is
     *  saved: a restart neither drops an unhandled request nor revives a
     *  decided one. 0 when nothing is saved. */
    public fun knockCursor(context: Context, accountIdB64: String?, groupIdHex: String): Long =
        accountIdB64?.let { read(context, key(KNOCK_CURSOR, it, groupIdHex))?.toLongOrNull() } ?: 0L

    /** Save [cursor] when it moves forward (never backwards). */
    public fun saveKnockCursor(context: Context, accountIdB64: String, groupIdHex: String, cursor: Long) {
        val next = knockCursorToSave(knockCursor(context, accountIdB64, groupIdHex), cursor) ?: return
        write(context, accountIdB64, key(KNOCK_CURSOR, accountIdB64, groupIdHex), next.toString())
    }

    /** ADR-0091 phase 1b — the highest MLS epoch the group's snapshot was at
     *  when its journal cursor advanced ([snapshotRewindFrom]); `null` when
     *  none is saved. */
    public fun cursorEpoch(context: Context, accountIdB64: String?, groupIdHex: String): ULong? =
        accountIdB64?.let { read(context, key(CURSOR_EPOCH, it, groupIdHex))?.toULongOrNull() }

    /** Save [epoch] when it is above the saved one (never backwards). */
    public fun saveCursorEpoch(context: Context, accountIdB64: String, groupIdHex: String, epoch: ULong) {
        val next = cursorEpochToSave(cursorEpoch(context, accountIdB64, groupIdHex), epoch) ?: return
        write(context, accountIdB64, key(CURSOR_EPOCH, accountIdB64, groupIdHex), next.toString())
    }

    /** Section 5 / ADR-0060 §6 — the reply grant (wire hex) a request to join
     *  this group carried for [accountHex], so it is cleared when the request
     *  is refused (an admitted person's grant stays until it expires). */
    public fun knockReply(context: Context, accountIdB64: String?, groupIdHex: String, accountHex: String): String? =
        accountIdB64?.let { read(context, key(KNOCK_REPLY + accountHex.lowercase() + ":", it, groupIdHex)) }

    public fun saveKnockReply(context: Context, accountIdB64: String, groupIdHex: String, accountHex: String, grantHex: String?) {
        write(context, accountIdB64, key(KNOCK_REPLY + accountHex.lowercase() + ":", accountIdB64, groupIdHex), grantHex)
    }

    /** 4.5.4 — whether the channel [channelIdHex]'s public record owes a
     *  republish (its name or description changed). */
    public fun owedChannelRecord(context: Context, accountIdB64: String?, channelIdHex: String): Boolean =
        accountIdB64 != null && read(context, key(OWED_CHANNEL_RECORD, accountIdB64, channelIdHex)) == "1"

    public fun saveOwedChannelRecord(context: Context, accountIdB64: String, channelIdHex: String, owed: Boolean) {
        write(context, accountIdB64, key(OWED_CHANNEL_RECORD, accountIdB64, channelIdHex), if (owed) "1" else null)
    }

    /** Section 8 — where a waiting request came from, by the joiner's key
     *  (hex), or `null` when this device never opened its row. */
    public fun knockOrigin(context: Context, accountIdB64: String?, groupIdHex: String, joinerHex: String): KnockOrigin? =
        accountIdB64?.let { decodeKnockOrigin(read(context, key(KNOCK_ORIGIN + joinerHex.lowercase() + ":", it, groupIdHex))) }

    /** Record where a request came from; the first record for a joiner stays. */
    public fun saveKnockOrigin(context: Context, accountIdB64: String, groupIdHex: String, joinerHex: String, origin: KnockOrigin) {
        val k = key(KNOCK_ORIGIN + joinerHex.lowercase() + ":", accountIdB64, groupIdHex)
        if (read(context, k) != null) return
        write(context, accountIdB64, k, encodeKnockOrigin(origin))
    }

    /** The group's stored photo, or `null`. */
    public fun photo(context: Context, accountIdB64: String?, groupIdHex: String): StoredGroupPhoto? =
        accountIdB64?.let { decodeGroupPhoto(read(context, key(PHOTO, it, groupIdHex))) }

    public fun savePhoto(context: Context, accountIdB64: String, groupIdHex: String, value: StoredGroupPhoto) {
        write(context, accountIdB64, key(PHOTO, accountIdB64, groupIdHex), encodeGroupPhoto(value))
    }

    /** The group's stored allowed reactions, or `null` (= all). */
    public fun reactions(context: Context, accountIdB64: String?, groupIdHex: String): StoredGroupReactions? =
        accountIdB64?.let { decodeGroupReactions(read(context, key(REACTIONS, it, groupIdHex))) }

    public fun saveReactions(context: Context, accountIdB64: String, groupIdHex: String, value: StoredGroupReactions) {
        write(context, accountIdB64, key(REACTIONS, accountIdB64, groupIdHex), encodeGroupReactions(value))
    }

    /** The group's stored handle, or `null`. */
    public fun handle(context: Context, accountIdB64: String?, groupIdHex: String): StoredGroupHandle? =
        accountIdB64?.let { decodeGroupHandle(read(context, key(HANDLE, it, groupIdHex))) }

    public fun saveHandle(context: Context, accountIdB64: String, groupIdHex: String, value: StoredGroupHandle) {
        write(context, accountIdB64, key(HANDLE, accountIdB64, groupIdHex), encodeGroupHandle(value))
    }

    /** The knock seed of [version] this device holds as an admin, or `null`. */
    public fun knockSeed(context: Context, accountIdB64: String?, groupIdHex: String, version: UShort): ByteArray? {
        val a = accountIdB64 ?: return null
        val raw = read(context, key(KNOCK + version + ":", a, groupIdHex)) ?: return null
        return try {
            b64d.decode(raw).takeIf { it.size == 32 }
        } catch (_: IllegalArgumentException) {
            null
        }
    }

    public fun saveKnockSeed(context: Context, accountIdB64: String, groupIdHex: String, version: UShort, seed: ByteArray) {
        write(context, accountIdB64, key(KNOCK + version + ":", accountIdB64, groupIdHex), b64e.encodeToString(seed))
    }

    /** 4.5.2 — a photo uploaded and not yet sent (the device was offline):
     *  shown with "Will apply when you're back online." and sent when it can. */
    public fun owedPhoto(context: Context, accountIdB64: String?, groupIdHex: String): StoredGroupPhoto? =
        accountIdB64?.let { decodeGroupPhoto(read(context, key(OWED_PHOTO, it, groupIdHex))) }

    public fun saveOwedPhoto(context: Context, accountIdB64: String, groupIdHex: String, value: StoredGroupPhoto?) {
        write(context, accountIdB64, key(OWED_PHOTO, accountIdB64, groupIdHex), value?.let(::encodeGroupPhoto))
    }

    /** Section 5 — the owner's own other devices are owed the knock seeds
     *  (after a bump or a rotation on this device), until sent. */
    public fun knockSelfOwed(context: Context, accountIdB64: String?, groupIdHex: String): Boolean =
        accountIdB64?.let { read(context, key(KNOCK_SELF_OWED, it, groupIdHex)) } != null

    public fun saveKnockSelfOwed(context: Context, accountIdB64: String, groupIdHex: String, owed: Boolean) {
        write(context, accountIdB64, key(KNOCK_SELF_OWED, accountIdB64, groupIdHex), if (owed) "1" else null)
    }

    /** Section 10 — this device created the group: its timeline opens with
     *  "You created this group", which stays after others join. */
    public fun created(context: Context, accountIdB64: String?, groupIdHex: String): Boolean =
        accountIdB64?.let { read(context, key(CREATED, it, groupIdHex)) } != null

    public fun markCreated(context: Context, accountIdB64: String, groupIdHex: String) {
        write(context, accountIdB64, key(CREATED, accountIdB64, groupIdHex), "1")
    }

    /** Section 3 — the OLD handle seed of a rekey owed after the owner's key
     *  rotation (SECRET), or `null`. */
    public fun owedRekeySeed(context: Context, accountIdB64: String?, groupIdHex: String): ByteArray? {
        val raw = accountIdB64?.let { read(context, key(OWED_REKEY, it, groupIdHex)) } ?: return null
        return try {
            b64d.decode(raw).takeIf { it.size == 32 }
        } catch (_: IllegalArgumentException) {
            null
        }
    }

    public fun saveOwedRekeySeed(context: Context, accountIdB64: String, groupIdHex: String, seed: ByteArray?) {
        write(context, accountIdB64, key(OWED_REKEY, accountIdB64, groupIdHex), seed?.let { b64e.encodeToString(it) })
    }

    /** Section 5 — the highest knock version derived from a predecessor's
     *  root (versions up to it are kept as stored seeds), or `null`. */
    public fun knockBoundary(context: Context, accountIdB64: String?, groupIdHex: String): UShort? =
        accountIdB64?.let { read(context, key(KNOCK_BOUNDARY, it, groupIdHex))?.toIntOrNull() }
            ?.takeIf { it in 0..0xFFFF }?.toUShort()

    public fun saveKnockBoundary(context: Context, accountIdB64: String, groupIdHex: String, version: UShort) {
        write(context, accountIdB64, key(KNOCK_BOUNDARY, accountIdB64, groupIdHex), version.toString())
    }

    /** Section 3 — when the owner's handle lease ends (unix ms), or `null`. */
    public fun leaseUntil(context: Context, accountIdB64: String?, groupIdHex: String): Long? =
        accountIdB64?.let { read(context, key(LEASE, it, groupIdHex))?.toLongOrNull() }

    public fun saveLeaseUntil(context: Context, accountIdB64: String, groupIdHex: String, untilMs: Long) {
        write(context, accountIdB64, key(LEASE, accountIdB64, groupIdHex), untilMs.toString())
    }

    /** Section 8 — a public / private change (or @name move) still owed:
     *  `(public, handle)`, retried until it lands. */
    public fun owedTypeChange(context: Context, accountIdB64: String?, groupIdHex: String): Pair<Boolean, String>? {
        val raw = accountIdB64?.let { read(context, key(OWED_TYPE, it, groupIdHex)) } ?: return null
        val (flag, handle) = raw.split(':', limit = 2).takeIf { it.size == 2 }?.let { it[0] to it[1] } ?: return null
        return (flag == "1") to handle
    }

    public fun saveOwedTypeChange(context: Context, accountIdB64: String, groupIdHex: String, change: Pair<Boolean, String>?) {
        write(
            context,
            accountIdB64,
            key(OWED_TYPE, accountIdB64, groupIdHex),
            change?.let { (if (it.first) "1" else "0") + ":" + it.second },
        )
    }

    /** Section 5 — the knock version new links and the record carry (0 until
     *  an admin was removed). */
    public fun knockVersion(context: Context, accountIdB64: String?, groupIdHex: String): UShort =
        accountIdB64?.let { read(context, key(KNOCK_VERSION, it, groupIdHex))?.toIntOrNull() }
            ?.takeIf { it in 0..0xFFFF }?.toUShort() ?: CURRENT_KNOCK_VERSION

    public fun saveKnockVersion(context: Context, accountIdB64: String, groupIdHex: String, version: UShort) {
        write(context, accountIdB64, key(KNOCK_VERSION, accountIdB64, groupIdHex), version.toString())
    }

    /** Section 5 — the admins (hex account keys) the owner last handed the
     *  current knock seed to; a removed one bumps the version. */
    public fun knockAdmins(context: Context, accountIdB64: String?, groupIdHex: String): Set<String>? =
        accountIdB64?.let { read(context, key(KNOCK_ADMINS, it, groupIdHex)) }?.split(',')?.filter { it.isNotBlank() }?.toSet()

    public fun saveKnockAdmins(context: Context, accountIdB64: String, groupIdHex: String, admins: Set<String>) {
        write(context, accountIdB64, key(KNOCK_ADMINS, accountIdB64, groupIdHex), admins.sorted().joinToString(","))
    }

    /** ADR-0087 18.5 — the blob ids of a replaced group photo this device
     *  uploaded and still owes a delete of (hex). */
    public fun owedDeletes(context: Context, accountIdB64: String?, groupIdHex: String): List<String> {
        val a = accountIdB64 ?: return emptyList()
        return read(context, key(OWED_DELETE, a, groupIdHex))?.split(',')?.filter { it.isNotBlank() }.orEmpty()
    }

    public fun saveOwedDeletes(context: Context, accountIdB64: String, groupIdHex: String, blobIdsHex: List<String>) {
        write(
            context,
            accountIdB64,
            key(OWED_DELETE, accountIdB64, groupIdHex),
            blobIdsHex.distinct().joinToString(",").ifEmpty { null },
        )
    }

    /** ADR-0087 section 9 — forget everything held for one group. */
    public fun deleteGroup(context: Context, accountIdB64: String, groupIdHex: String) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val p = try {
                prefs(context)
            } catch (_: Throwable) {
                return@write
            }
            val suffix = ":" + accountIdB64 + ":" + groupIdHex.lowercase()
            val e = p.edit()
            for (k in p.all.keys) if (k.endsWith(suffix)) e.remove(k)
            e.apply()
            mutableVersion.value += 1
        }
    }

    /** Forget every setting held for [accountIdB64] (forget device). */
    public fun deleteAccount(context: Context, accountIdB64: String) {
        val p = try {
            prefs(context)
        } catch (_: Throwable) {
            return
        }
        val e = p.edit()
        for (k in p.all.keys) if (k.contains(":$accountIdB64:")) e.remove(k)
        e.apply()
        mutableVersion.value += 1
    }

    /** Every group of [accountIdB64] with a stored photo (the keep source). */
    public fun photoGroups(context: Context, accountIdB64: String): List<String> {
        val p = try {
            prefs(context)
        } catch (_: Throwable) {
            return emptyList()
        }
        val prefix = PHOTO + accountIdB64 + ":"
        return p.all.keys.filter { it.startsWith(prefix) }.map { it.removePrefix(prefix) }
    }
}

/** The knock-key version of a group nobody has bumped yet (ADR-0087
 *  section 5); the current one is [GroupSettingsStore.knockVersion]. */
public const val CURRENT_KNOCK_VERSION: UShort = 0u

/** Section 5 — the knock version after the admin set moved from [previous]
 *  to [current]: one higher when an admin was removed, else unchanged. Pure. */
public fun nextKnockVersion(previous: Set<String>?, current: Set<String>, version: UShort): UShort =
    if (previous != null && previous.any { it !in current } && version < UShort.MAX_VALUE) {
        (version + 1u).toUShort()
    } else {
        version
    }

/** Where the handle write key sits in a handle certificate:
 *  `version(1) ‖ lookup_key(64) ‖ write_pk(32) ‖ issued_at(8) ‖ sig(64)`. */
internal const val HANDLE_CERT_WRITE_KEY_START: Int = 65
internal const val HANDLE_CERT_WRITE_KEY_END: Int = 97

/**
 * `docs/ux/group-and-channel-privacy.md` section 8 — where one request to join
 * came from: when its knock row was posted (the DS stamp) and, for a request
 * made from a `/g/` link, who issued the link (the capability's issuer key).
 */
public data class KnockOrigin(val askedAtMs: Long?, val issuerKeyB64: String?)

internal fun encodeKnockOrigin(o: KnockOrigin): String =
    (o.askedAtMs?.toString() ?: "") + "|" + (o.issuerKeyB64 ?: "")

internal fun decodeKnockOrigin(raw: String?): KnockOrigin? {
    if (raw.isNullOrEmpty()) return null
    val i = raw.indexOf('|')
    if (i < 0) return null
    return KnockOrigin(
        askedAtMs = raw.substring(0, i).toLongOrNull(),
        issuerKeyB64 = raw.substring(i + 1).takeIf { it.isNotEmpty() },
    )
}

/**
 * Section 8 — the line under a request: `Invited by {name} · {when}` from a
 * link, `Asked {when}` otherwise; without a time, `Invited by {name}`, and no
 * line at all for a request with neither.
 */
public fun joinRequestLine(
    origin: KnockOrigin?,
    attributionIssuerB64: String?,
    nameOf: (String) -> String,
    whenOf: (Long) -> String,
): me.lakko.app.l10n.UiText? {
    val issuer = origin?.issuerKeyB64 ?: attributionIssuerB64
    val time = origin?.askedAtMs?.let(whenOf)
    return when {
        issuer != null && time != null ->
            me.lakko.app.l10n.UiText("chat.members.join.invited_by", mapOf("name" to nameOf(issuer), "when" to time))
        issuer != null ->
            me.lakko.app.l10n.UiText("chat.members.join.invited_by_name", mapOf("name" to nameOf(issuer)))
        time != null -> me.lakko.app.l10n.UiText("chat.members.join.asked", mapOf("when" to time))
        else -> null
    }
}

/** The knock position to save: [next] when it moves past [saved], else
 *  `null` (a position never moves backwards). */
public fun knockCursorToSave(saved: Long, next: Long): Long? = next.takeIf { it > saved }
