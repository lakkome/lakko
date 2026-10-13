// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.reach

/*
 * ADR-0060 — who may reach this person, as pure decisions.
 *
 * Everything here is free of the native core so the whole rule set runs on the
 * host JVM: the classes are mirrored as [ReachKind], keys are compared as hex,
 * and the one derivation that needs the core (a reach seed's public key) is a
 * function the caller passes in. [ReachManager] is the only place these meet
 * the FFI and the Delivery Service.
 */

/** Who a reach key is handed to (ADR-0060 §1). Mirror of `FfiReachClass`. */
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

/** How long a `reply` key lives (ADR-0060 §1): seven days, chosen, not measured. */
public const val REPLY_TTL_MS: Long = 7L * 24 * 60 * 60 * 1000

/** Byte length of a random `reply` scope (ADR-0060 §1). */
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
 * The key set the owner's mailbox should hold at [nowMs] (ADR-0060 §1, the
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
 * Whether to keep a reach grant a peer handed out (ADR-0060 client contract,
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
 * (ADR-0060 §6), or `null` for none (the write then goes unsigned, which an
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
): HeldGrantRef? = orderedWriteGrants(held, purpose, groupHex).firstOrNull()

/**
 * Every grant held for one person that may authorise a write of [purpose], in
 * the order [pickWriteGrant] prefers them (within one class, by scope), each
 * once. ADR-0050 section 2.5a item 7: a Welcome writer tries them in this
 * order and moves to the next on a `401` (the person rotated that key away),
 * so a stale grant never hides a live one held under another class or scope.
 */
public fun orderedWriteGrants(
    held: Collection<HeldGrantRef>,
    purpose: WritePurpose,
    groupHex: String? = null,
): List<HeldGrantRef> {
    fun all(kind: ReachKind, scope: String? = null): List<HeldGrantRef> =
        held.filter { it.kind == kind && (scope == null || it.scopeHex == scope) }
            .sortedBy { it.scopeHex }
    val order: List<() -> List<HeldGrantRef>> = when (purpose) {
        WritePurpose.CONTACT_REQUEST -> listOf(
            { all(ReachKind.CONTACT) },
            { all(ReachKind.GROUP) },
            { all(ReachKind.CHANNEL) },
            { all(ReachKind.FOF) },
        )
        WritePurpose.REPLY -> listOf(
            { all(ReachKind.REPLY) },
            { all(ReachKind.CONTACT) },
        )
        WritePurpose.WELCOME -> listOf(
            { all(ReachKind.REPLY) },
            { all(ReachKind.CONTACT) },
            { groupHex?.let { all(ReachKind.MEMBER, it) }.orEmpty() },
            { groupHex?.let { all(ReachKind.GROUP, it) }.orEmpty() },
            { all(ReachKind.GROUP) },
            { all(ReachKind.CHANNEL) },
            { all(ReachKind.FOF) },
        )
    }
    val out = LinkedHashSet<HeldGrantRef>()
    for (candidate in order) out.addAll(candidate())
    return out.toList()
}

/** What [firstAcceptedWrite] ended with. */
public sealed class SignedWriteOutcome<out T> {
    /** A candidate's write was stored. */
    public data class Written<T>(val result: T) : SignedWriteOutcome<T>()

    /** No candidate was accepted: none existed ([lastRefusal] `null`), or each
     *  was refused with a `401`. Nothing was written. */
    public data class Exhausted(val lastRefusal: Throwable?) : SignedWriteOutcome<Nothing>()
}

/**
 * ADR-0050 section 2.5a item 7 — try each authorisation in [candidates] in
 * order: a `401` ([isRefusal]) moves to the next one, anything else
 * propagates. A candidate that yields `null` (its signer could not be built)
 * is skipped. Never writes without a candidate: what to do when every
 * candidate is spent is the caller's call.
 */
public suspend fun <A, T> firstAcceptedWrite(
    candidates: List<() -> A?>,
    write: suspend (A) -> T,
    isRefusal: (Throwable) -> Boolean,
): SignedWriteOutcome<T> {
    var lastRefusal: Throwable? = null
    for (candidate in candidates) {
        val auth = try {
            candidate()
        } catch (_: Throwable) {
            null
        } ?: continue
        try {
            return SignedWriteOutcome.Written(write(auth))
        } catch (cause: Throwable) {
            if (cause is kotlinx.coroutines.CancellationException || !isRefusal(cause)) throw cause
            lastRefusal = cause
        }
    }
    return SignedWriteOutcome.Exhausted(lastRefusal)
}

// -----------------------------------------------------------------
// Versions after a restore (ADR-0060 §1, amended 2026-10-01)
// -----------------------------------------------------------------

/**
 * The highest version a device with no version state tries per slot when it
 * recovers versions from the keys its mailbox holds.
 *
 * Chosen, not measured. A slot's version moves by one per rotation: a removed
 * contact (`contact`, `fof`), a group that shrank (`group`, `member` of that
 * group), and for `channel` it follows the channel epoch. 256 rotations of one
 * slot is far past what a person accumulates through removals, while the scan
 * stays at 257 key derivations per slot, run once per slot per restore in the
 * background. The residual: a slot rotated more than 256 times (in practice a
 * long-lived channel's epoch) finds no registered key in range and starts at
 * 0, which is what the ADR prescribes for a class with no registered key; a
 * channel's slot is then raised to the channel epoch on the next member card.
 */
public const val MAX_RECOVERED_REACH_VERSION: Int = 256

/** Where a document stands on recovering its versions from the server. */
public enum class VersionRecovery {
    /** The device held no version state: until the owner-only reach read
     *  succeeds nothing versioned is registered. */
    PENDING,

    /** Not needed (the device held version state), or done. */
    DONE,
}

/**
 * Whether a document holds no version state at all — a fresh install or a
 * restore with no other device alive: no slot was ever rotated, nothing was
 * ever registered from here, and no ReachState arrived from a sibling. Only
 * such a device recovers versions; any other trusts its own counters.
 */
public fun holdsNoVersionState(versionsEmpty: Boolean, registeredEmpty: Boolean, reachStateKnown: Boolean): Boolean =
    versionsEmpty && registeredEmpty && !reachStateKnown

/** A slot's stable spelling in the document (`KIND:scopeHex`). */
public fun reachSlotId(slot: ReachSlot): String = slot.kind.name + ":" + slot.scopeHex

/**
 * The slots a restored device can name and recover: `contact` and `fof`
 * (empty scope), `group` and `member` of each group it holds, `channel` of
 * each channel it holds. `link` and `reply` are per capability or single use
 * and are never recovered.
 */
public fun recoverableSlots(joinedGroups: Collection<String>, channels: Collection<String>): List<ReachSlot> {
    val out = ArrayList<ReachSlot>()
    out += ReachSlot(ReachKind.CONTACT, "")
    out += ReachSlot(ReachKind.FOF, "")
    for (group in joinedGroups.map { it.lowercase() }.distinct().sorted()) {
        out += ReachSlot(ReachKind.MEMBER, group)
        out += ReachSlot(ReachKind.GROUP, group)
    }
    for (channel in channels.map { it.lowercase() }.distinct().sorted()) out += ReachSlot(ReachKind.CHANNEL, channel)
    return out
}

/**
 * The version [slot] resumes at: the HIGHEST version in `0..bound` whose
 * public key ([publicKeyHexAt], lowercase hex) is in [registered], or `null`
 * when none is (the slot then starts at 0). Taking the highest means nothing
 * lower is ever registered again, so a key rotated away (a removed contact,
 * an evicted member) never comes back.
 */
public fun recoverSlotVersion(
    slot: ReachSlot,
    registered: Set<String>,
    publicKeyHexAt: (ReachSlot, Int) -> String?,
    bound: Int = MAX_RECOVERED_REACH_VERSION,
): Int? {
    if (registered.isEmpty()) return null
    var found: Int? = null
    for (version in 0..bound) {
        val pk = publicKeyHexAt(slot, version) ?: continue
        if (pk in registered) found = version
    }
    return found
}

/** [recoverSlotVersion] for each of [slots]; only the slots with a
 *  registered key appear. */
public fun recoverVersions(
    slots: Collection<ReachSlot>,
    registered: Set<String>,
    publicKeyHexAt: (ReachSlot, Int) -> String?,
    bound: Int = MAX_RECOVERED_REACH_VERSION,
): Map<ReachSlot, Int> {
    val out = LinkedHashMap<ReachSlot, Int>()
    for (slot in slots) recoverSlotVersion(slot, registered, publicKeyHexAt, bound)?.let { out[slot] = it }
    return out
}

/**
 * What may be registered while a recovery is [VersionRecovery.PENDING]: only
 * the keys that carry no version history (`link`, `reply`), so an invite link
 * or an outgoing request still works. A versioned key could be one a rotation
 * removed, so none goes out until the server read tells which is current.
 */
public fun registrableBeforeRecovery(desired: List<DesiredReachKey>): List<DesiredReachKey> =
    desired.filter { it.slot.kind == ReachKind.LINK || it.slot.kind == ReachKind.REPLY }

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
 * ADR-0060 §3, "Shrink" — whether a membership change obliges this account to
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

/** What a bind does with the registrations a document records. */
public enum class ReachRekey {
    /** They were made under the bound key: nothing changes. */
    KEEP,

    /** Nothing says which key they were made under and nothing says the key
     *  changed: they are the bound key's, and the document now says so. */
    STAMP,

    /** They were made under ANOTHER key of this account, hence on another
     *  mailbox: they are forgotten here and everything is registered again
     *  under the bound key. The old mailbox is left as it is. */
    MOVE,
}

/**
 * ADR-0074 section 3.5, "Reach" - a reach key and the mailbox it is
 * registered on both derive from the account seed, so after a rotation of
 * the account keys the registrations a device recorded describe the OLD
 * mailbox. [boundKeyHex] is the account key being bound now,
 * [registeredForKeyHex] the key the stored registrations are recorded under
 * (`null` when never recorded), [previousKeyHex] the key this same account
 * was bound under in this process until now (`null` on a first bind). Pure.
 */
public fun rekeyDecision(boundKeyHex: String, registeredForKeyHex: String?, previousKeyHex: String?): ReachRekey =
    when {
        registeredForKeyHex == boundKeyHex -> ReachRekey.KEEP
        registeredForKeyHex != null -> ReachRekey.MOVE
        previousKeyHex != null && previousKeyHex != boundKeyHex -> ReachRekey.MOVE
        else -> ReachRekey.STAMP
    }
