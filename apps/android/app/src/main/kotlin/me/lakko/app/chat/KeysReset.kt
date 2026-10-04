// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context
import android.content.SharedPreferences
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import me.lakko.app.l10n.UiText
import uniffi.lakko_ffi_uniffi.GroupKind
import uniffi.lakko_ffi_uniffi.Role

/**
 * ADR-0065 phase 2 / `docs/ux/identity-recovery.md` 16.3 - 16.5 — what a
 * shared conversation does when one of its people reset their account keys.
 *
 * The contact module records the reset ([me.lakko.app.contacts.ResetRecord]);
 * this module decides, per conversation, which of the three branches applies
 * and carries the two durable records the branches need: the eviction
 * bookkeeping of 16.3 and the closed state of a one-to-one (16.4). The
 * group-closed envelope of 16.5 lives in `GroupClosed.kt`.
 *
 * Every rule is a pure function over plain values (the host suite renders no
 * composable and loads no native library); `ChatRepository` feeds it live
 * group state and `ChatScreen` renders its answers.
 */

/** The branch one conversation falls in for one flagged contact (16.3 / 16.4 / 16.5). */
public sealed interface KeysResetBranch {
    /** A role-table group the old identity did not own, judged by an owner or
     *  admin: propose the removal of every leaf resolving to the old key. */
    public data object EvictionProposal : KeysResetBranch

    /** The same group seen by a plain member: the flag on the row, nothing
     *  proposed (a member may not propose another account's removal). */
    public data object FlagOnly : KeysResetBranch

    /** A one-to-one cannot lose a person (ADR-0062): the chat closes. */
    public data object OneToOneClose : KeysResetBranch

    /** The old identity OWNED the group: no eviction (`owner_survives`), the
     *  owned-group notice and `Recreate group` for an admin. */
    public data object OwnedGroupNotice : KeysResetBranch

    /** A group with no role table: no proposal path, so only the flag. */
    public data object NoRoleTableFlag : KeysResetBranch
}

/**
 * Decide the branch.
 *
 * @param kind the conversation's ADR-0062 kind; `null` reads as a group.
 * @param hasRoleTable whether the group carries an ADR-0022 role table.
 * @param ownerIsPrevious whether the role table's owner key is the contact's
 *   previous (reset) key.
 * @param ownRole this device's own role, `null` without a role table.
 */
public fun keysResetBranch(
    kind: GroupKind?,
    hasRoleTable: Boolean,
    ownerIsPrevious: Boolean,
    ownRole: Role?,
): KeysResetBranch {
    if (kind == GroupKind.ONE_TO_ONE) return KeysResetBranch.OneToOneClose
    if (!hasRoleTable) return KeysResetBranch.NoRoleTableFlag
    if (ownerIsPrevious) return KeysResetBranch.OwnedGroupNotice
    return when (ownRole) {
        Role.OWNER, Role.ADMIN -> KeysResetBranch.EvictionProposal
        else -> KeysResetBranch.FlagOnly
    }
}

/**
 * The `applied` test (`proposal-based-membership.md` section 4) on the
 * previous key's device leaves: the eviction has applied once no leaf of the
 * roster resolves to the previous key any more. A torn roster read answers
 * `false` — nothing is reported removed that was not seen removed.
 */
public fun evictionApplied(rosterAccountKeysB64: List<String>?, previousKeyB64: String): Boolean =
    rosterAccountKeysB64 != null && rosterAccountKeysB64.none { it == previousKeyB64 }

/**
 * 16.8 — the scheduled re-admission arm's ADD branch is suppressed for an
 * account whose contact is reset-unverified, or whose published key is not
 * the pin. Without it every member's client would propose adding the new
 * identity's devices to every shared group within ten minutes of the reset,
 * ahead of any verification.
 */
public fun suppressReadmissionAdd(
    contactUnverified: Boolean,
    publishedKeyIsPinned: Boolean?,
): Boolean = contactUnverified || publishedKeyIsPinned == false

/**
 * 16.4 — a late message into a closed one-to-one (an old device that had not
 * yet learned of the reset) is dropped before it is stored. True exactly when
 * the conversation is closed.
 */
public fun dropIncomingChatRow(conversationClosed: Boolean): Boolean = conversationClosed

/** 16.3 — the members-panel secondary line of the old-key member's row. */
public fun resetMemberLine(branch: KeysResetBranch): UiText = when (branch) {
    KeysResetBranch.EvictionProposal -> UiText("chat.keys_reset.member_being_removed")
    else -> UiText("chat.keys_reset.member_flag")
}

/**
 * 16.1 — the warning under a bubble from the NEW keys: shown to a member who
 * has the person flagged (their contact, not yet verified), never to one who
 * verified them and never to one who was never their contact.
 */
public fun bubbleWarningFor(senderKeyB64: String?, flaggedPendingKeysB64: Set<String>): Boolean =
    senderKeyB64 != null && senderKeyB64 in flaggedPendingKeysB64

/** 16.1 — the warning line's text. */
public fun bubbleWarningText(name: String): UiText =
    UiText("chat.keys_reset.bubble_warning", texts = mapOf("name" to name))

/**
 * 16.3 — the system row when the sweep lands: the reset's own sentence on a
 * device that knows the reset, the existing `Members changed` row otherwise.
 */
public fun evictionLandedRow(knowsReset: Boolean, name: String?): UiText =
    if (knowsReset && name != null) {
        UiText("chat.keys_reset.system_removed", texts = mapOf("name" to name))
    } else {
        UiText("chat.system.members_changed")
    }

/** 16.5 — the owned-group notice above the pending notice. */
public data class OwnedGroupNotice(
    val text: UiText,
    /** The plain-member tail line, `null` for an admin. */
    val memberLine: UiText?,
    /** Whether `Recreate group` is offered (an admin, or the owner of nothing — never the owner, who is the one reset). */
    val canRecreate: Boolean,
)

public fun ownedGroupNotice(ownerName: String, ownRole: Role?): OwnedGroupNotice {
    val text = UiText("chat.keys_reset.owned_notice", texts = mapOf("name" to ownerName))
    val canRecreate = ownRole == Role.ADMIN
    return OwnedGroupNotice(
        text = text,
        memberLine = if (canRecreate) null else UiText("chat.keys_reset.owned_notice_member"),
        canRecreate = canRecreate,
    )
}

/** 16.4 — the closed one-to-one's shapes. */
public object OneToOneClosedCopy {
    public fun terminalHeadline(name: String): UiText =
        UiText("chat.keys_reset.one_to_one_closed", texts = mapOf("name" to name))

    public fun listRow(name: String): UiText =
        UiText("chat.keys_reset.one_to_one_row", texts = mapOf("name" to name))

    public val COMPOSER: UiText = UiText("chat.keys_reset.one_to_one_composer")
    public val START_NEW_CHAT: UiText = UiText("chat.keys_reset.start_new_chat")
}

/**
 * 16.4 — `Start a new chat` is disabled until the contact is verified, with
 * the helper line and the `Open contact` link; enabled afterwards.
 */
public data class StartNewChatAffordance(
    val enabled: Boolean,
    val helper: UiText?,
    val openContact: UiText?,
)

public fun startNewChatAffordance(contactUnverified: Boolean, name: String?): StartNewChatAffordance =
    if (contactUnverified) {
        StartNewChatAffordance(
            enabled = false,
            helper = me.lakko.app.contacts.KeysResetCopy.verifyFirst(name),
            openContact = me.lakko.app.contacts.KeysResetCopy.OPEN_CONTACT,
        )
    } else {
        StartNewChatAffordance(enabled = true, helper = null, openContact = null)
    }

// ---------------------------------------------------------------------------
// Durable records
// ---------------------------------------------------------------------------

/** Why a conversation is closed on this device. */
public enum class ClosedReason {
    /** 16.4 — the other person of a one-to-one reset their account keys. */
    PEER_KEYS_RESET,

    /** 16.5 — an admin closed the group after the owner's keys were reset. */
    GROUP_CLOSED_BY_ADMIN,
}

/** A conversation's closed state (16.4 / 16.5), persisted per `(account, group)`. */
public data class ConversationClosed(
    val reason: ClosedReason,
    val closedAtMs: Long,
    /** 16.4 — the contact's account id, standard base64, for `Start a new chat`. */
    val peerAccountIdB64: String? = null,
    /** 16.5 — the admin's claimed successor group, hex; `Open the new group`
     *  only once this device has joined it. */
    val successorGroupIdHex: String? = null,
    /** 16.5 — the closer's account key, standard base64, for the terminal row. */
    val closerKeyB64: String? = null,
)

@Serializable
private data class StoredClosed(
    val reason: String,
    val closedAtMs: Long,
    val peerAccountIdB64: String? = null,
    val successorGroupIdHex: String? = null,
    val closerKeyB64: String? = null,
)

private val closedJson = Json { ignoreUnknownKeys = true }

internal fun encodeClosed(value: ConversationClosed): String =
    closedJson.encodeToString(
        StoredClosed.serializer(),
        StoredClosed(value.reason.name, value.closedAtMs, value.peerAccountIdB64, value.successorGroupIdHex, value.closerKeyB64),
    )

internal fun decodeClosed(raw: String?): ConversationClosed? {
    if (raw.isNullOrEmpty()) return null
    return try {
        val row = closedJson.decodeFromString(StoredClosed.serializer(), raw)
        val reason = ClosedReason.entries.firstOrNull { it.name == row.reason } ?: return null
        ConversationClosed(reason, row.closedAtMs, row.peerAccountIdB64, row.successorGroupIdHex, row.closerKeyB64)
    } catch (_: Exception) {
        null
    }
}

/** The per-`(account, group)` closed-state store. Plain preferences: a reason,
 *  a stamp, an account id and a group id — no key material. */
public object ConversationClosedStore {
    private const val PREFS = "lakko-conversation-closed"
    private const val KEY_PREFIX = "closed-v1:"

    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun key(accountIdB64: String, groupIdHex: String): String =
        KEY_PREFIX + accountIdB64 + ":" + groupIdHex.lowercase()

    public fun load(context: Context, accountIdB64: String?, groupIdHex: String): ConversationClosed? {
        if (accountIdB64 == null) return null
        return try {
            decodeClosed(prefs(context).getString(key(accountIdB64, groupIdHex), null))
        } catch (_: Throwable) {
            null
        }
    }

    public fun save(context: Context, accountIdB64: String, groupIdHex: String, value: ConversationClosed) {
        prefs(context).edit().putString(key(accountIdB64, groupIdHex), encodeClosed(value)).apply()
    }

    public fun remove(context: Context, accountIdB64: String, groupIdHex: String) {
        prefs(context).edit().remove(key(accountIdB64, groupIdHex)).apply()
    }

    public fun deleteAccount(context: Context, accountIdB64: String) {
        try {
            val p = prefs(context)
            val prefix = KEY_PREFIX + accountIdB64 + ":"
            val e = p.edit()
            for (k in p.all.keys) if (k.startsWith(prefix)) e.remove(k)
            e.apply()
        } catch (_: Throwable) {
            // best-effort
        }
    }
}

/** 16.8 — per group, per flagged contact: the eviction proposed, when, and whether it applied. */
public data class EvictionRecord(
    val previousKeyB64: String,
    val proposedAtMs: Long?,
    val applied: Boolean,
)

@Serializable
private data class StoredEviction(
    val previousKeyB64: String,
    val proposedAtMs: Long? = null,
    val applied: Boolean = false,
)

internal fun encodeEviction(value: EvictionRecord): String =
    closedJson.encodeToString(StoredEviction.serializer(), StoredEviction(value.previousKeyB64, value.proposedAtMs, value.applied))

internal fun decodeEviction(raw: String?): EvictionRecord? {
    if (raw.isNullOrEmpty()) return null
    return try {
        val row = closedJson.decodeFromString(StoredEviction.serializer(), raw)
        if (row.previousKeyB64.isEmpty()) return null
        EvictionRecord(row.previousKeyB64, row.proposedAtMs, row.applied)
    } catch (_: Exception) {
        null
    }
}

/**
 * The per-`(account, group, previous key)` eviction bookkeeping. The chat's
 * poll marks a record applied when the previous key's leaves are gone from the
 * roster; the re-request discipline proposes again on open while it is not.
 */
public object EvictionStore {
    private const val PREFS = "lakko-keys-reset-evictions"
    private const val KEY_PREFIX = "eviction-v1:"

    private fun prefs(context: Context): SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun prefix(accountIdB64: String, groupIdHex: String): String =
        KEY_PREFIX + accountIdB64 + ":" + groupIdHex.lowercase() + ":"

    private fun key(accountIdB64: String, groupIdHex: String, previousKeyB64: String): String =
        prefix(accountIdB64, groupIdHex) + previousKeyB64

    public fun load(context: Context, accountIdB64: String, groupIdHex: String, previousKeyB64: String): EvictionRecord? =
        try {
            decodeEviction(prefs(context).getString(key(accountIdB64, groupIdHex, previousKeyB64), null))
        } catch (_: Throwable) {
            null
        }

    public fun loadForGroup(context: Context, accountIdB64: String, groupIdHex: String): List<EvictionRecord> =
        try {
            val p = prefix(accountIdB64, groupIdHex)
            prefs(context).all.entries.filter { it.key.startsWith(p) }.mapNotNull { decodeEviction(it.value as? String) }
        } catch (_: Throwable) {
            emptyList()
        }

    public fun save(context: Context, accountIdB64: String, groupIdHex: String, record: EvictionRecord) {
        prefs(context).edit().putString(key(accountIdB64, groupIdHex, record.previousKeyB64), encodeEviction(record)).apply()
    }

    public fun deleteAccount(context: Context, accountIdB64: String) {
        try {
            val p = prefs(context)
            val prefix = KEY_PREFIX + accountIdB64 + ":"
            val e = p.edit()
            for (k in p.all.keys) if (k.startsWith(prefix)) e.remove(k)
            e.apply()
        } catch (_: Throwable) {
            // best-effort
        }
    }
}

/**
 * 16.3 — should the eviction be proposed now? At the moment the reset is
 * noticed, and again on open for any group where it has not applied; never
 * once applied. A record with a proposal already made this session is left to
 * the re-request discipline (the pending set holds the request).
 */
public fun shouldProposeEviction(record: EvictionRecord?, proposedThisSession: Boolean): Boolean {
    if (record != null && record.applied) return false
    return !proposedThisSession
}
