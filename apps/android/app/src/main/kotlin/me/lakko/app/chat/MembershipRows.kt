// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import me.lakko.app.l10n.Localization
import me.lakko.app.l10n.Strings

/**
 * `docs/ux/second-device.md` section 4, "Membership rows (decided
 * 2026-10-29)": there is no generic "Members changed" row. In a GROUP, a
 * receive batch whose commits change the PEOPLE (ADR-0050 person keys)
 * leaves one row at the batch's last commit, naming each person: "VG3
 * joined", "VG2 left". The row stores person keys, never names; names are
 * resolved when the row is drawn. A one-to-one never has such a row
 * (ADR-0062), and this device's own joining or removal writes none.
 */
public data class MembershipChange(
    /** Person keys (standard base64) that joined, one per person. */
    val joined: List<MemberIdentity>,
    /** Person keys (standard base64) that left, one per person. */
    val left: List<MemberIdentity>,
)

/**
 * The people a batch added and removed, or `null` when it changed none.
 *
 * @param personKeysBefore the roster's person keys before the batch, `null`
 *   on a torn read (then nothing is recorded).
 * @param personKeysAfter the same after the batch.
 * @param accountOfKey the person a key belongs to across an accepted reset
 *   (a superseded key and its successor are one person), `null` when the key
 *   names only itself — the mapping [deviceSwapAcrossCommits] uses.
 * @param swapSuppressed the device-swap verdict on this batch: a reset's
 *   devices changing, across one or two commits, is no change of people.
 * @param ownPersonKey this account's person key: its own joining or removal
 *   writes no row here.
 */
public fun membershipChangeOf(
    personKeysBefore: List<String>?,
    personKeysAfter: List<String>?,
    accountOfKey: (String) -> String?,
    swapSuppressed: Boolean,
    ownPersonKey: String?,
): MembershipChange? {
    if (swapSuppressed || personKeysBefore == null || personKeysAfter == null) return null
    if (personKeysBefore.toSet() == personKeysAfter.toSet()) return null
    val person = { k: String -> accountOfKey(k) ?: k }
    val own = ownPersonKey?.let(person)
    val peopleBefore = peopleByAccount(personKeysBefore, accountOfKey)
    val peopleAfter = peopleByAccount(personKeysAfter, accountOfKey)
    val joined = peopleAfter.filter { it !in peopleBefore && it != own }
        .mapNotNull { p -> personKeysAfter.firstOrNull { person(it) == p } }
    val left = peopleBefore.filter { it !in peopleAfter && it != own }
        .mapNotNull { p -> personKeysBefore.firstOrNull { person(it) == p } }
    if (joined.isEmpty() && left.isEmpty()) return null
    return MembershipChange(joined = joined, left = left)
}

/**
 * Which event of a receive batch carries [change]: the batch's LAST commit,
 * or `-1` when nothing is recorded (no change, the commit rows suppressed,
 * or no commit in the batch).
 */
public fun membershipRowIndex(
    events: List<IncomingChatEvent>,
    change: MembershipChange?,
    commitRowsSuppressed: Boolean,
): Int {
    if (change == null || commitRowsSuppressed) return -1
    return events.indexOfLast { it is IncomingChatEvent.System && it.systemKind == SystemKind.Commit }
}

/**
 * This device's own merged commit, as the row a receiver would have written
 * from it: a commit row at the commit's DS message id naming [change].
 */
public fun ownCommitRow(messageId: Long, createdAt: String, change: MembershipChange): ChatMessage =
    ChatMessage(
        messageId = messageId,
        direction = Direction.System,
        text = COMMIT_ROW_TEXT,
        createdAt = createdAt,
        membershipJoined = change.joined,
        membershipLeft = change.left,
    )

/**
 * Where [ownCommitRow] goes in a timeline held in journal order: before the
 * first server row with a higher id (rows this device has not sent yet, id
 * `0`, and local call rows, negative ids, are not journal rows), else at the
 * end. `null` when a row with that id is already held.
 */
public fun ownCommitRowInsertIndex(messages: List<ChatMessage>, messageId: Long): Int? {
    if (messages.any { it.messageId == messageId && it.historyOf == null }) return null
    val at = messages.indexOfFirst { it.historyOf == null && it.messageId > messageId }
    return if (at < 0) messages.size else at
}

/** The text a stored commit row carries (`ChatRepository.systemRowText`). */
internal const val COMMIT_ROW_TEXT: String = "[mls commit]"

/** The text a stored external-join request row carries. */
internal const val EXTERNAL_JOIN_ROW_TEXT: String = "[mls external join]"

/** Whether [message] is a stored commit row that names people. */
public fun isMembershipRow(message: ChatMessage): Boolean =
    message.direction == Direction.System && message.call == null &&
        message.text == COMMIT_ROW_TEXT &&
        (message.membershipJoined.isNotEmpty() || message.membershipLeft.isNotEmpty())

/**
 * The drawn line of a membership row: one sentence per person, joined
 * first, each name resolved now through [nameOf] (the one display resolver,
 * "Member" when unknown). `null` for any other row.
 */
public fun membershipRowText(
    message: ChatMessage,
    nameOf: (MemberIdentity) -> String,
    strings: Strings = Localization.strings.value,
): String? {
    if (!isMembershipRow(message)) return null
    val sentences = message.membershipJoined.map { strings.format("chat.system.member_joined", mapOf("name" to nameOf(it))) } +
        message.membershipLeft.map { strings.format("chat.system.member_left", mapOf("name" to nameOf(it))) }
    return sentences.joinToString(" ")
}
