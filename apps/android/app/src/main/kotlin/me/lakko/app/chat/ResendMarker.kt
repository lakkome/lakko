// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

/*
 * ADR-0073 R.7 case 1 - the RE-SEND MARKER.
 *
 * The founder of a successor one-to-one sends again, into it, its own
 * account's messages that reached only removed devices ([messagesToResend]).
 * Each re-sent message carries, inside the sealed envelope, where it was
 * originally sent: the ORIGINAL group id and the original DS message id there
 * (`composeChatEnvelope` with `resendOriginGroupB64` / `resendOriginMessageId`).
 *
 * The person who reset holds no history, so for them the re-sent message is
 * simply a message. A device that DOES hold the original (another device of
 * the founder's own account) would otherwise show it twice. This rule makes
 * it show once: the successor row is not drawn, not stored, not counted and
 * not notified, and the history row stays where it is and is not replaced.
 *
 * The marker is a claim by the sender, so it is believed only against what
 * this device already holds: the named row must exist, in an OLDER segment of
 * the SAME conversation, and be by the SAME author account as the sender the
 * MLS layer authenticated. A marker that matches nothing is ignored and the
 * message renders as any other. Nothing is ever drawn for the marker itself.
 * Mirror of `apps/web/src/chat/resend-marker.ts`.
 */

/** Where a re-sent message was originally sent. */
public data class ResendOrigin(
    /** The original conversation group's id, standard base64. */
    val groupIdB64: String,
    /** The original's DS `messageId` in that group. */
    val messageId: Long,
)

public enum class ResendVerdict {
    /** An ordinary message: stored, shown, counted. */
    DRAW,

    /** This device already holds the original: the successor row is dropped
     *  before storage. */
    SKIP,
}

/**
 * | marker | origin group is an older segment of the receiving group's conversation | this device holds the named row | its author is the sender's account | verdict |
 * |--------|-------------------------------------------------------------------------|---------------------------------|------------------------------------|---------|
 * | none   | -                                                                       | -                               | -                                  | `DRAW`  |
 * | set    | no (another conversation, an unknown group, the same or a later group)  | -                               | -                                  | `DRAW`  |
 * | set    | yes                                                                     | no                              | -                                  | `DRAW`  |
 * | set    | yes                                                                     | yes                             | no                                 | `DRAW`  |
 * | set    | yes                                                                     | yes                             | yes                                | `SKIP`  |
 */
public fun resendVerdict(
    marker: ResendOrigin?,
    /** The conversation the message was RECEIVED in. */
    receivedConversation: String,
    /** The conversation the marker's group belongs to, or `null` when this
     *  device holds no such group. */
    originConversation: String?,
    /** Whether the marker's group comes BEFORE the receiving group in that
     *  conversation's segments. */
    originIsOlderSegment: Boolean,
    /** The author account of the row the marker names, or `null` when this
     *  device does not hold that row. */
    heldAuthor: String?,
    /** The account of the MLS-authenticated sender of the received message. */
    senderAuthor: String?,
): ResendVerdict {
    if (marker == null) return ResendVerdict.DRAW
    if (originConversation == null || originConversation != receivedConversation) return ResendVerdict.DRAW
    if (!originIsOlderSegment) return ResendVerdict.DRAW
    if (heldAuthor == null || senderAuthor == null) return ResendVerdict.DRAW
    return if (heldAuthor == senderAuthor) ResendVerdict.SKIP else ResendVerdict.DRAW
}

/** How a stored row was produced, as the marker rule reads it. */
public enum class HeldRowDirection { OUTGOING, INCOMING, SYSTEM }

/**
 * The author account token of a stored row: this account's own key for a row
 * this device sent, the stored author key (through [canonical]) for a
 * received one, `null` for a system row, a row with no author, or no row.
 */
public fun heldRowAuthor(
    direction: HeldRowDirection?,
    authorKeyB64: String?,
    ownPersonKeyB64: String?,
    canonical: (String) -> String,
): String? = when (direction) {
    null, HeldRowDirection.SYSTEM -> null
    HeldRowDirection.OUTGOING -> ownPersonKeyB64?.let(canonical)
    HeldRowDirection.INCOMING -> authorKeyB64?.takeIf { it.isNotEmpty() }?.let(canonical)
}

/** The marker a parsed body carries: both fields or nothing. */
public fun resendOriginFrom(groupIdB64: String?, messageId: Long?): ResendOrigin? =
    if (groupIdB64.isNullOrEmpty() || messageId == null) null else ResendOrigin(groupIdB64, messageId)

/** A stored row as the marker rule needs it: how it was produced and by whom. */
public data class HeldRow(val direction: HeldRowDirection, val authorKeyB64: String?)

/**
 * [resendVerdict] for a reader that holds no live group records, only what
 * the app stored: the lineage and the message log. The background push
 * reader is one (C-59: it reads, and writes nothing).
 *
 * [heldRowOf] answers for `(group hex, message id)` from the stored log, or
 * `null` when this device holds no such row (or no such group). Keys are
 * compared as they are: the founder re-sends its OWN account's messages, so
 * the row this device holds is one its own account sent, and the sender the
 * MLS layer authenticated is that same account.
 */
public fun storedResendVerdict(
    marker: ResendOrigin?,
    receivedGroupHex: String,
    lineage: Lineage,
    heldRowOf: (groupHex: String, messageId: Long) -> HeldRow?,
    ownPersonKeyB64: String?,
    senderKeyB64: String?,
): ResendVerdict {
    if (marker == null) return ResendVerdict.DRAW
    val originHex = groupHexOfB64(marker.groupIdB64)
    if (originHex.isEmpty() || marker.messageId <= 0) return ResendVerdict.DRAW
    val received = receivedGroupHex.lowercase()
    val row = heldRowOf(originHex, marker.messageId)
    return resendVerdict(
        marker = marker,
        receivedConversation = lineage.conversationOf(received),
        originConversation = if (row == null) null else lineage.conversationOf(originHex),
        originIsOlderSegment = lineage.segmentIndexOf(originHex) < lineage.segmentIndexOf(received),
        heldAuthor = heldRowAuthor(row?.direction, row?.authorKeyB64, ownPersonKeyB64) { it },
        senderAuthor = senderKeyB64,
    )
}

/** The direction a stored row's `direction` text names, or `null`. */
public fun heldRowDirectionOf(stored: String?): HeldRowDirection? = when (stored?.lowercase()) {
    "outgoing" -> HeldRowDirection.OUTGOING
    "incoming" -> HeldRowDirection.INCOMING
    "system" -> HeldRowDirection.SYSTEM
    else -> null
}

/** The row `(group, message id)` of a stored message log, or `null`. */
public fun heldRowIn(log: MessageLogPayload?, groupHex: String, messageId: Long): HeldRow? {
    if (log == null || messageId <= 0) return null
    val key = groupHex.lowercase()
    val group = log.groups[key] ?: log.groups.entries.firstOrNull { it.key.lowercase() == key }?.value ?: return null
    val row = group.messages.firstOrNull { it.messageId == messageId } ?: return null
    val direction = heldRowDirectionOf(row.direction) ?: return null
    return HeldRow(direction, row.authorKeyB64)
}

/*
 * ADR-0101 section 9 with ADR-0073 R.7 case 1 (live walk 2026-11-26, D3) -
 * a re-sent message and the history page's copy of the same message.
 *
 * The person back after a key reset holds no older segment, so [resendVerdict]
 * always draws a re-sent message there. Its history then comes back through
 * the returned conversation, filed into the SAME group under the original DS
 * id (DS ids are unique across journals, so that id can name nothing else),
 * and the message showed twice. The re-send's marker names exactly that id.
 * So, in the group a message is RECEIVED in:
 *
 * - a re-send whose marker names a row this group already holds, by the same
 *   author account as the sender, is not drawn ([resendDuplicatesHeldRow]);
 * - a history row whose id a drawn re-send of the same author named is not
 *   filed ([historyRowAlreadyResent]); the drawn re-sends are recorded
 *   ([DrawnResendStore]) because the re-send usually arrives first.
 *
 * Authors are compared through `canonical` (a retired key names the person's
 * current key), so a sender can only ever suppress its own earlier message.
 */

/** A re-sent message this device drew: the original DS id its marker named
 *  and its sender's account key (canonical, standard base64). */
public data class DrawnResend(val originMessageId: Long, val senderKeyB64: String)

/**
 * Whether a re-send ([marker], from [senderAuthor], canonical) duplicates a row
 * the RECEIVING group already holds ([heldInReceived]: the row whose id the
 * marker names there, or `null`). Pure.
 */
public fun resendDuplicatesHeldRow(
    marker: ResendOrigin?,
    heldInReceived: HeldRow?,
    ownPersonKeyB64: String?,
    senderAuthor: String?,
    canonical: (String) -> String,
): Boolean {
    if (marker == null || marker.messageId <= 0 || heldInReceived == null || senderAuthor == null) return false
    val held = heldRowAuthor(heldInReceived.direction, heldInReceived.authorKeyB64, ownPersonKeyB64, canonical) ?: return false
    return held == senderAuthor
}

/**
 * Whether a history row ([messageId], authored by [authorKeyB64]) is the
 * original of a re-send this device already drew in the same group. Pure.
 */
public fun historyRowAlreadyResent(
    messageId: Long,
    authorKeyB64: String?,
    drawn: Collection<DrawnResend>,
    canonical: (String) -> String,
): Boolean {
    if (messageId <= 0 || authorKeyB64.isNullOrEmpty() || drawn.isEmpty()) return false
    val author = canonical(authorKeyB64)
    return drawn.any { it.originMessageId == messageId && canonical(it.senderKeyB64) == author }
}

/** The re-sends this device drew, per signed-in account and group (plain:
 *  message ids and public keys). */
public class DrawnResendStore(private val storage: me.lakko.app.auth.MarkerStorage) {
    private val json = kotlinx.serialization.json.Json { ignoreUnknownKeys = true }
    private val serializer = kotlinx.serialization.builtins.MapSerializer(
        kotlinx.serialization.serializer<String>(),
        kotlinx.serialization.builtins.ListSerializer(kotlinx.serialization.serializer<String>()),
    )
    private val cache = java.util.concurrent.ConcurrentHashMap<String, Map<String, List<String>>>()

    private fun load(account: String): Map<String, List<String>> =
        cache.getOrPut(account) {
            val raw = runCatching { storage.get(KEY_PREFIX + account) }.getOrNull()
            raw?.let { runCatching { json.decodeFromString(serializer, it) }.getOrNull() } ?: emptyMap()
        }

    /** The re-sends drawn in [groupIdHex]. */
    public fun drawn(account: String, groupIdHex: String): List<DrawnResend> =
        load(account)[groupIdHex.lowercase()].orEmpty().mapNotNull { entry ->
            val cut = entry.indexOf('|')
            if (cut <= 0) return@mapNotNull null
            val id = entry.substring(0, cut).toLongOrNull() ?: return@mapNotNull null
            DrawnResend(id, entry.substring(cut + 1))
        }

    /** Record one (once). Never throws. */
    public fun record(account: String, groupIdHex: String, resend: DrawnResend) {
        if (resend.originMessageId <= 0 || resend.senderKeyB64.isEmpty()) return
        val hex = groupIdHex.lowercase()
        val entry = "${resend.originMessageId}|${resend.senderKeyB64}"
        val held = load(account)
        val rows = held[hex].orEmpty()
        if (entry in rows) return
        val next = held + (hex to (rows + entry).takeLast(MAX_PER_GROUP))
        cache[account] = next
        try {
            me.lakko.app.account.AccountStoreGate.write(account) {
                storage.set(KEY_PREFIX + account, json.encodeToString(serializer, next))
            }
        } catch (_: Throwable) {
            // Kept in memory for this run: a history page filed after a restart
            // may show the message twice, never lose it.
        }
    }

    public companion object {
        public const val KEY_PREFIX: String = "resend-drawn:"

        /** How many are kept per group (chosen). */
        public const val MAX_PER_GROUP: Int = 512

        public fun onDevice(context: android.content.Context): DrawnResendStore =
            DrawnResendStore(me.lakko.app.auth.ResetReturnStorage.plain(context))
    }
}
