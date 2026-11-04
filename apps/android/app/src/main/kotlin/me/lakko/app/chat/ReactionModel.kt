// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * docs/ux/reactions-and-emoji.md sections 10 and 17 — a message's reactions as
 * the ops that reached it, and what a person holds after the fold.
 *
 * A reaction op says ADD or REMOVE (no toggle), names its person by the
 * ACCOUNT key the sender leaf resolved to at receipt (ADR-0050, C-68), and is
 * ordered by its DS message id, which every member sees alike. What is held is
 * the core's fold (`foldReactions`): an add of an emoji already held and a
 * remove of one not held are no-ops, and an add past three per person is
 * ignored for good. The fold is written once in Rust; this file keeps the ops
 * and threads the fold in through [ReactionFold], so the bookkeeping runs on
 * the host JVM and the rule itself is never re-implemented here.
 */

/** One reaction op on one message, as this device received or sent it. */
public data class ReactionOpRecord(
    /** The person's account key, standard base64. */
    val person: MemberIdentity,
    val emoji: String,
    /** `true` = add, `false` = take off. */
    val add: Boolean,
    /** The op's DS message id; ops apply in ascending order. */
    val at: Long,
)

/** The core's fold, injectable for host tests. Returns what is held, or
 *  `null` when the fold could not run (the caller keeps what it had). */
public fun interface ReactionFold {
    public fun fold(ops: List<ReactionOpRecord>): List<Reaction>?
}

private val b64d = java.util.Base64.getDecoder()
private val b64e = java.util.Base64.getEncoder()

/** The FFI fold. A person key that is not 32 bytes of base64 is left out. */
public val NativeReactionFold: ReactionFold = ReactionFold { ops ->
    try {
        val ffiOps = ops.mapNotNull { op ->
            val person = try {
                b64d.decode(op.person)
            } catch (_: IllegalArgumentException) {
                null
            }?.takeIf { it.size == 32 } ?: return@mapNotNull null
            uniffi.lakko_ffi_uniffi.FfiReactionOp(
                person = person,
                emoji = op.emoji,
                change = if (op.add) uniffi.lakko_ffi_uniffi.ReactionAction.ADD else uniffi.lakko_ffi_uniffi.ReactionAction.REMOVE,
                at = op.at,
            )
        }
        uniffi.lakko_ffi_uniffi.foldReactions(ffiOps).map {
            Reaction(reactor = b64e.encodeToString(it.person), emoji = it.emoji, at = it.at)
        }
    } catch (_: Throwable) {
        null
    }
}

/**
 * Add [op] to [ops] in DS order. An op already held (same person, emoji, change
 * and id) is not added twice, so a re-delivered row changes nothing. Pure.
 */
public fun appendReactionOp(ops: List<ReactionOpRecord>, op: ReactionOpRecord): List<ReactionOpRecord> {
    if (ops.any { it == op }) return ops
    val out = ArrayList<ReactionOpRecord>(ops.size + 1)
    out.addAll(ops)
    out.add(op)
    out.sortWith(compareBy({ it.at }, { it.person }, { it.emoji }))
    return out
}

/** The emoji [person] holds on a message. Pure. */
public fun ownReactionEmoji(held: List<Reaction>, person: MemberIdentity?): List<String> =
    if (person == null) emptyList() else held.filter { it.reactor == person }.map { it.emoji }

/** The kinds a message holds, in the order they were first given. Pure. */
public fun reactionKinds(held: List<Reaction>): List<String> =
    held.sortedBy { it.at }.map { it.emoji }.distinct()

/** reactions-and-emoji section 12 — whether a held reaction is a new one by
 *  someone else on one of [ownKey]'s messages (an unseen reaction). Pure. */
public fun othersReactedTo(held: List<Reaction>, ownKey: MemberIdentity?): Boolean =
    ownKey != null && held.any { it.reactor != ownKey }

/** A stamp for an op this device applies without a DS id (a dead one-to-one,
 *  ADR-0073 R.7 case 1): after every op held. Pure. */
public fun localOpId(ops: List<ReactionOpRecord>): Long = (ops.maxOfOrNull { it.at } ?: 0L) + 1L

/**
 * reactions-and-emoji D3 / D4 — the account's quick reaction, as the synced
 * state's register holds it (projected by the DeviceGroup sync), else the
 * core's default. Written through `DeviceGroupSyncRepository.setQuickReaction`.
 */
public object QuickReaction {
    private val mutable = kotlinx.coroutines.flow.MutableStateFlow<String?>(null)

    /** The chosen quick reaction, `null` while never chosen. */
    public val chosen: kotlinx.coroutines.flow.StateFlow<String?> = mutable

    /** Called by the sync projection. */
    public fun held(value: String?) {
        mutable.value = value
    }

    /** The quick reaction to send now. */
    public fun current(): String = mutable.value ?: try {
        uniffi.lakko_ffi_uniffi.defaultQuickReaction()
    } catch (_: Throwable) {
        "👍"
    }
}

/** reactions-and-emoji section 12 — this device's `Reactions` notification
 *  switch, on by default (D8). A device setting, not synced. */
public object ReactionNotifyPref {
    private const val PREFS = "lakko-reaction-notify"
    private const val KEY = "on"

    public fun isOn(context: android.content.Context): Boolean =
        context.applicationContext.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE)
            .getBoolean(KEY, true)

    public fun set(context: android.content.Context, on: Boolean) {
        context.applicationContext.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE)
            .edit().putBoolean(KEY, on).apply()
    }
}

/**
 * reactions-and-emoji section 12 — per conversation, this person's own
 * messages that others reacted to and that have not been on screen since.
 * Local to the device, persisted beside the message log for the bound
 * account so a restart keeps it. [version] moves on every change.
 */
public object UnseenReactions {
    private const val PREFS = "lakko-unseen-reactions"
    private val byGroup = java.util.concurrent.ConcurrentHashMap<String, MutableSet<Long>>()
    private val mutableVersion = kotlinx.coroutines.flow.MutableStateFlow(0L)

    @Volatile
    private var bound: Pair<android.content.Context, String>? = null

    /** Moves on every change. */
    public val version: kotlinx.coroutines.flow.StateFlow<Long> = mutableVersion

    /** Load [accountIdB64]'s unseen set (the repository binds the account). */
    @Synchronized
    public fun bind(context: android.content.Context, accountIdB64: String) {
        if (bound?.second == accountIdB64) return
        bound = context.applicationContext to accountIdB64
        byGroup.clear()
        val raw = context.applicationContext.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE)
            .getString(accountIdB64, null)
        byGroup.putAll(decodeUnseen(raw))
        mutableVersion.value += 1
    }

    /** Forget the bound account (sign-out). */
    @Synchronized
    public fun unbind() {
        bound = null
        byGroup.clear()
        mutableVersion.value += 1
    }

    private fun persist() {
        val (ctx, account) = bound ?: return
        val raw = encodeUnseen(byGroup)
        me.lakko.app.account.AccountStoreGate.write(account) {
            ctx.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE).edit().putString(account, raw).apply()
        }
    }

    /** Note a new reaction by someone else on own message [messageId]. */
    public fun mark(groupIdHex: String, messageId: Long) {
        val set = byGroup.getOrPut(groupIdHex.lowercase()) { java.util.concurrent.ConcurrentHashMap.newKeySet() }
        if (set.add(messageId)) {
            persist()
            mutableVersion.value += 1
        }
    }

    /** The message was on screen. */
    public fun seen(groupIdHex: String, messageId: Long) {
        if (byGroup[groupIdHex.lowercase()]?.remove(messageId) == true) {
            persist()
            mutableVersion.value += 1
        }
    }

    /** The unseen messages of a conversation, oldest first. */
    public fun of(groupIdHex: String): List<Long> = byGroup[groupIdHex.lowercase()]?.sorted().orEmpty()

    /** Whether a conversation has any (the chat-list heart). */
    public fun any(groupIdHex: String): Boolean = byGroup[groupIdHex.lowercase()]?.isNotEmpty() == true

    /** Forget a conversation (its end) or everything. */
    public fun clear(groupIdHex: String? = null) {
        if (groupIdHex == null) byGroup.clear() else byGroup.remove(groupIdHex.lowercase())
        persist()
        mutableVersion.value += 1
    }

    /** Forget device. */
    public fun deleteAccount(context: android.content.Context, accountIdB64: String) {
        context.applicationContext.getSharedPreferences(PREFS, android.content.Context.MODE_PRIVATE)
            .edit().remove(accountIdB64).apply()
        if (bound?.second == accountIdB64) unbind()
    }
}

/** `group:id,id;group:id` — the persisted unseen set. Pure. */
internal fun encodeUnseen(map: Map<String, Set<Long>>): String =
    map.filterValues { it.isNotEmpty() }.entries.joinToString(";") { (g, ids) -> g + ":" + ids.sorted().joinToString(",") }

/** The inverse of [encodeUnseen]; anything malformed is skipped. Pure. */
internal fun decodeUnseen(raw: String?): Map<String, MutableSet<Long>> {
    if (raw.isNullOrEmpty()) return emptyMap()
    val out = HashMap<String, MutableSet<Long>>()
    for (part in raw.split(';')) {
        val (g, ids) = part.split(':', limit = 2).takeIf { it.size == 2 }?.let { it[0] to it[1] } ?: continue
        val set: MutableSet<Long> = java.util.concurrent.ConcurrentHashMap.newKeySet()
        ids.split(',').mapNotNullTo(set) { it.toLongOrNull() }
        if (set.isNotEmpty() && g.isNotEmpty()) out[g] = set
    }
    return out
}
