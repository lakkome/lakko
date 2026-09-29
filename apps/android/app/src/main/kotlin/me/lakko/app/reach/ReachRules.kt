// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

/*
 * ADR-0058 — who may reach this person, as pure decisions.
 *
 * Everything here is free of the native core so the whole rule set runs on the
 * host JVM: the classes are mirrored as [ReachKind], keys are compared as hex,
 * and the one derivation that needs the core (a reach seed's public key) is a
 * function the caller passes in. [ReachManager] is the only place these meet
 * the FFI and the Delivery Service.
 */

/** Who a reach key is handed to (ADR-0058 §1). Mirror of `FfiReachClass`. */
public enum class ReachKind {
    /** Every contact (sent in the 1:1 conversation). Empty scope. */
    CONTACT,

    /** Whoever holds one invite link. Scope: the link id. */
    LINK,

    /** The one party a request of the owner's went to. Scope: a random id. */
    REPLY,

    /** Members of a group the owner lets find them. Scope: the group id. */
    GROUP,

    /** Every co-member of a group, whatever the setting. Scope: the group id. */
    MEMBER,

    /** Members of a channel the owner lets find them. Scope: the channel id. */
    CHANNEL,

    /** Contacts of contacts. Empty scope. */
    FOF,
    ;

    /** Whether the class is derived over an empty scope. */
    public val scopeless: Boolean get() = this == CONTACT || this == FOF
}

/** How long a `reply` key lives (ADR-0058 §1): seven days, chosen, not measured. */
public const val REPLY_TTL_MS: Long = 7L * 24 * 60 * 60 * 1000

/** Byte length of a random `reply` scope (ADR-0058 §1). */
public const val REPLY_SCOPE_LEN: Int = 16

/** One of the owner's key slots: a class and a scope (lowercase hex, `""` for
 *  the scopeless classes). A slot's key changes only when its version does. */
public data class ReachSlot(val kind: ReachKind, val scopeHex: String)

/** A key the owner wants registered: its slot, version and optional expiry. */
public data class DesiredReachKey(
    val slot: ReachSlot,
    val version: Int,
    val expiresAtMs: Long?,
)

/** A key the owner registered, as this device remembers it. */
public data class RegisteredReachKey(
    val slot: ReachSlot,
    val version: Int,
    val expiresAtMs: Long?,
)

/**
 * Everything the owner's key set is computed from.
 *
 * @param versions the version of every slot that was ever rotated; an absent
 *   slot is at version 0.
 * @param joinedGroups every conversation group this account is in (hex): each
 *   carries a `member` key, whatever the setting.
 * @param visibleGroups the groups the setting lets find the owner (a subset of
 *   [joinedGroups] under "all", the chosen ones otherwise).
 * @param visibleChannels the channels the setting lets find the owner.
 * @param contactsOfContacts whether part 2.1 is on (a `fof` key).
 * @param links every live invite link this account minted: link id (hex) to
 *   its expiry (`null` = never).
 * @param replies every outgoing request still waiting: reply scope (hex) to
 *   its expiry.
 */
public data class ReachInputs(
    val versions: Map<ReachSlot, Int>,
    val joinedGroups: Set<String>,
    val visibleGroups: Set<String>,
    val visibleChannels: Set<String>,
    val contactsOfContacts: Boolean,
    val links: Map<String, Long?>,
    val replies: Map<String, Long>,
)

/**
 * The key set the owner's mailbox should hold at [nowMs] (ADR-0058 §1, the
 * client contract "Reach keys the owner holds").
 *
 * `contact` is always there (a contact can always write); `member` for every
 * joined group; `group` / `channel` only where the setting allows; `fof` only
 * with part 2.1 on; a link key per live link; a reply key per waiting request.
 * An expired link or reply contributes nothing, so the diff removes its key.
 */
public fun desiredReachKeys(inputs: ReachInputs, nowMs: Long): List<DesiredReachKey> {
    fun version(slot: ReachSlot): Int = inputs.versions[slot] ?: 0
    val out = ArrayList<DesiredReachKey>()
    fun add(kind: ReachKind, scopeHex: String, expiresAtMs: Long?) {
        val slot = ReachSlot(kind, scopeHex)
        out += DesiredReachKey(slot, version(slot), expiresAtMs)
    }
    add(ReachKind.CONTACT, "", null)
    if (inputs.contactsOfContacts) add(ReachKind.FOF, "", null)
    for (group in inputs.joinedGroups.sorted()) add(ReachKind.MEMBER, group, null)
    for (group in inputs.visibleGroups.filter { it in inputs.joinedGroups }.sorted()) {
        add(ReachKind.GROUP, group, null)
    }
    for (channel in inputs.visibleChannels.sorted()) add(ReachKind.CHANNEL, channel, null)
    for ((link, expiry) in inputs.links.toSortedMap()) {
        if (expiry != null && expiry <= nowMs) continue
        add(ReachKind.LINK, link, expiry)
    }
    for ((reply, expiry) in inputs.replies.toSortedMap()) {
        if (expiry <= nowMs) continue
        add(ReachKind.REPLY, reply, expiry)
    }
    return out
}

/** One `PUT /ds/mailbox/{hex}/reach` worth of change, keys as hex. */
public data class ReachDiff(
    /** Public key (hex) -> the key it registers. */
    val add: Map<String, DesiredReachKey>,
    /** Public keys (hex) to unregister. */
    val remove: Set<String>,
) {
    public val isEmpty: Boolean get() = add.isEmpty() && remove.isEmpty()
}

/**
 * What to send so the mailbox holds exactly [desired] (keyed by the public key
 * each derives to) given what this device believes is [registered].
 *
 * A key already registered with the same expiry is not re-sent; one whose
 * expiry changed is re-added (the server replaces the expiry); anything
 * registered and no longer desired is removed.
 */
public fun diffReachKeys(
    desired: Map<String, DesiredReachKey>,
    registered: Map<String, RegisteredReachKey>,
): ReachDiff {
    val add = LinkedHashMap<String, DesiredReachKey>()
    for ((pk, key) in desired) {
        val have = registered[pk]
        if (have == null || have.expiresAtMs != key.expiresAtMs) add[pk] = key
    }
    val remove = registered.keys.filterNot { it in desired }.toSortedSet()
    return ReachDiff(add = add, remove = remove)
}

/** Where a reach grant was received. */
public enum class GrantVenue {
    /** A one-to-one conversation's profile. */
    ONE_TO_ONE,

    /** A group conversation's profile. */
    GROUP,

    /** A channel member card. */
    CHANNEL_CARD,
}

/**
 * Whether to keep a reach grant a peer handed out (ADR-0058 client contract,
 * the last write-path rule): `contact` ONLY from the other participant of a
 * one-to-one conversation; `group` / `member` only from a member of that group
 * (and `member` also in a one-to-one, which is a group of two); `channel` only
 * from that channel's member cards. `link`, `reply` and `fof` never ride a
 * profile and are dropped.
 *
 * [senderKey] is the sender's signature key RESOLVED AT RECEIPT (C-68): the
 * account key the MLS sender leaf named at the epoch the envelope was sealed
 * at. `null` (unresolvable) and this device's own account are refused.
 */
public fun acceptReceivedGrant(
    kind: ReachKind,
    venue: GrantVenue,
    senderKeyHex: String?,
    selfKeyHex: String,
    otherParticipantKeyHex: String?,
    senderIsMember: Boolean,
): Boolean {
    if (senderKeyHex.isNullOrEmpty()) return false
    if (senderKeyHex == selfKeyHex) return false
    return when (kind) {
        ReachKind.CONTACT ->
            venue == GrantVenue.ONE_TO_ONE &&
                otherParticipantKeyHex != null &&
                senderKeyHex == otherParticipantKeyHex
        ReachKind.MEMBER ->
            (venue == GrantVenue.GROUP || venue == GrantVenue.ONE_TO_ONE) && senderIsMember
        ReachKind.GROUP -> venue == GrantVenue.GROUP && senderIsMember
        ReachKind.CHANNEL -> venue == GrantVenue.CHANNEL_CARD
        ReachKind.LINK, ReachKind.REPLY, ReachKind.FOF -> false
    }
}

/** A grant this device holds for someone else: its class and scope (the
 *  scope is where it was received — a group id for `group` / `member`, a
 *  channel id for `channel`, `""` otherwise). */
public data class HeldGrantRef(val kind: ReachKind, val scopeHex: String)

/** What a write into someone's mailbox is. */
public enum class WritePurpose {
    /** A contact request: the person must have let this device find them. */
    CONTACT_REQUEST,

    /** A reply to their own request (the 1:1 Welcome after an accept, the
     *  ContactAck): their `reply` key first. */
    REPLY,

    /** A Welcome into a group ([groupHex]). */
    WELCOME,
}

/**
 * Which of the grants held for one person authorises a write of [purpose]
 * (ADR-0058 §6), or `null` for none (the write then goes unsigned, which an
 * open mailbox takes and a closed one drops).
 *
 * - A contact request never uses `member`: a co-member the person hides from
 *   holds it, and it authorises only Welcomes (the ADR's residual).
 * - A reply prefers the person's `reply` key, then `contact`.
 * - A Welcome prefers `reply`, `contact`, then `member` of THAT group (the
 *   ADR-0050 re-admission of the person's new device), then anything that
 *   says the person lets this device find them.
 */
public fun pickWriteGrant(
    held: Collection<HeldGrantRef>,
    purpose: WritePurpose,
    groupHex: String? = null,
): HeldGrantRef? {
    fun first(kind: ReachKind, scope: String? = null): HeldGrantRef? =
        held.filter { it.kind == kind && (scope == null || it.scopeHex == scope) }
            .minByOrNull { it.scopeHex }
    val order: List<() -> HeldGrantRef?> = when (purpose) {
        WritePurpose.CONTACT_REQUEST -> listOf(
            { first(ReachKind.CONTACT) },
            { first(ReachKind.GROUP) },
            { first(ReachKind.CHANNEL) },
            { first(ReachKind.FOF) },
        )
        WritePurpose.REPLY -> listOf(
            { first(ReachKind.REPLY) },
            { first(ReachKind.CONTACT) },
        )
        WritePurpose.WELCOME -> listOf(
            { first(ReachKind.REPLY) },
            { first(ReachKind.CONTACT) },
            { groupHex?.let { first(ReachKind.MEMBER, it) } },
            { groupHex?.let { first(ReachKind.GROUP, it) } },
            { first(ReachKind.GROUP) },
            { first(ReachKind.CHANNEL) },
            { first(ReachKind.FOF) },
        )
    }
    for (candidate in order) candidate()?.let { return it }
    return null
}

/** The reply scopes (hex) whose seven days have run out at [nowMs]. */
public fun expiredReplies(replies: Map<String, Long>, nowMs: Long): Set<String> =
    replies.filterValues { it <= nowMs }.keys

/** The expiry of a reply key minted at [nowMs]. */
public fun replyExpiry(nowMs: Long): Long = nowMs + REPLY_TTL_MS

/** Lowercase hex of [bytes]. */
public fun reachHex(bytes: ByteArray): String {
    val sb = StringBuilder(bytes.size * 2)
    for (b in bytes) {
        val v = b.toInt() and 0xFF
        sb.append(HEX_DIGITS[v ushr 4])
        sb.append(HEX_DIGITS[v and 0x0F])
    }
    return sb.toString()
}

/** Bytes of lowercase / uppercase hex, or `null` when malformed. */
public fun reachUnhex(hex: String): ByteArray? {
    if (hex.length % 2 != 0) return null
    val out = ByteArray(hex.length / 2)
    for (i in out.indices) {
        val hi = Character.digit(hex[2 * i], 16)
        val lo = Character.digit(hex[2 * i + 1], 16)
        if (hi < 0 || lo < 0) return null
        out[i] = ((hi shl 4) or lo).toByte()
    }
    return out
}

private const val HEX_DIGITS: String = "0123456789abcdef"

/**
 * ADR-0058 §3, "Shrink" — whether a membership change obliges this account to
 * rotate its `group` and `member` keys for the group: someone left (a
 * signature key present before is absent after — a set difference, so a
 * remove-and-add in one batch still counts), the roster read was not torn
 * (`null` / empty), and it was not this device that left (an evicted device
 * has nobody left to hand the new key to).
 */
public fun reachRotationOnShrink(
    previousMemberKeys: Collection<String>?,
    currentMemberKeys: Collection<String>?,
    selfEvicted: Boolean,
): Boolean {
    if (selfEvicted) return false
    if (previousMemberKeys.isNullOrEmpty() || currentMemberKeys.isNullOrEmpty()) return false
    val now = currentMemberKeys.toHashSet()
    return previousMemberKeys.any { it !in now }
}
