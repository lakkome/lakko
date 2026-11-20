// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import kotlinx.serialization.Serializable
import uniffi.lakko_ffi_uniffi.ChecklistItemRef
import uniffi.lakko_ffi_uniffi.MessageBodyKind
import uniffi.lakko_ffi_uniffi.MessageContent

/**
 * ADR-0092 — the ops a poll or checklist row collects. Never a bubble: each is
 * kept on its target row and the core's fold turns them into what is drawn.
 */
public enum class ContentOpKind {
    PollVote,
    PollClose,
    ChecklistTick,
    ChecklistAdd,
    EditChecklist,
    EditArticle,
}

internal fun contentOpKindOf(kind: MessageBodyKind): ContentOpKind = when (kind) {
    MessageBodyKind.POLL_VOTE -> ContentOpKind.PollVote
    MessageBodyKind.POLL_CLOSE -> ContentOpKind.PollClose
    MessageBodyKind.CHECKLIST_TICK -> ContentOpKind.ChecklistTick
    MessageBodyKind.CHECKLIST_ADD -> ContentOpKind.ChecklistAdd
    MessageBodyKind.EDIT_CHECKLIST -> ContentOpKind.EditChecklist
    else -> ContentOpKind.EditArticle
}

/**
 * One op as the receiver recorded it. [sender] is the account key resolved at
 * receipt (C-68); [privileged] is whether that account was the owner or an
 * admin of the live role table at receipt, which the core's fold reads for a
 * close and for a tick or add on a checklist that does not allow others.
 */
public data class ContentOpRecord(
    val kind: ContentOpKind,
    val opId: Long,
    val createdAtMs: Long,
    val sender: MemberIdentity,
    val privileged: Boolean,
    val choices: ByteArray? = null,
    val item: ChecklistItemRef? = null,
    val done: Boolean? = null,
    val text: String = "",
    /** An edit's replacement content (checklist or article). */
    val content: MessageContent? = null,
    val attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef> = emptyList(),
) {
    override fun equals(other: Any?): Boolean =
        other is ContentOpRecord && other.kind == kind && other.opId == opId && other.sender == sender
    override fun hashCode(): Int = (opId * 31 + kind.ordinal).toInt()
}

/** ADR-0092 section 3.6 — a ballot row read from the journal, kept on its poll. */
public data class BallotRecord(val messageId: Long, val createdAtMs: Long, val row: ByteArray) {
    override fun equals(other: Any?): Boolean = other is BallotRecord && other.messageId == messageId
    override fun hashCode(): Int = messageId.hashCode()
}

/** UX 7.5 — the device's own anonymous vote while it is on its way. */
public enum class OwnBallotStatus { Sending, Sent, Failed }

public data class OwnBallot(
    val choices: ByteArray,
    val status: OwnBallotStatus,
    val setVersion: UInt? = null,
) {
    override fun equals(other: Any?): Boolean =
        other is OwnBallot && other.choices.contentEquals(choices) && other.status == status &&
            other.setVersion == setVersion
    override fun hashCode(): Int = choices.contentHashCode() * 31 + status.hashCode()
}

/** The state of one row's content that is not its body. */
public data class ContentExtras(
    val ops: List<ContentOpRecord> = emptyList(),
    val ballots: List<BallotRecord> = emptyList(),
    /** [uniffi.lakko_ffi_uniffi.PollRing.encode] of an anonymous poll's ring. */
    val ring: ByteArray? = null,
    val ownBallot: OwnBallot? = null,
    /** ADR-0092 section 3.11 — the state a history bundle or the vault
     *  carried for a row this device has no ops of: drawn as is. */
    val snapshot: uniffi.lakko_ffi_uniffi.ContentState? = null,
) {
    override fun equals(other: Any?): Boolean =
        other is ContentExtras && other.ops == ops && other.ballots == ballots && other.snapshot == snapshot &&
            (other.ring?.contentEquals(ring ?: ByteArray(0)) ?: (ring == null)) && other.ownBallot == ownBallot
    override fun hashCode(): Int = ops.hashCode() * 31 + ballots.hashCode()
}

// ---------------------------------------------------------------- persistence

@Serializable
public data class PersistedContentOp(
    val kind: String,
    val opId: Long,
    val createdAtMs: Long,
    val sender: String,
    val privileged: Boolean,
    val choicesB64: String? = null,
    val itemId: Long? = null,
    val itemMessageId: Long? = null,
    val done: Boolean? = null,
    val text: String = "",
    val contentEnvelopeB64: String? = null,
)

@Serializable
public data class PersistedBallot(val messageId: Long, val createdAtMs: Long, val rowB64: String)

@Serializable
public data class PersistedContent(
    /** The content as a composed content envelope, re-parsed on load. */
    val envelopeB64: String,
    val ops: List<PersistedContentOp> = emptyList(),
    val ballots: List<PersistedBallot> = emptyList(),
    val ringB64: String? = null,
    val ownBallotChoicesB64: String? = null,
    val ownBallotStatus: String? = null,
    val ownBallotSetVersion: Long? = null,
    val snapshot: PersistedSnapshot? = null,
)

/** [ContentExtras.snapshot] as stored. */
@Serializable
public data class PersistedSnapshot(
    val pollCounts: List<Long>? = null,
    val pollVoters: Long = 0,
    /** "open", "time", or "op:<messageId>". */
    val pollClosed: String = "open",
    val checklistClosedBy: Long? = null,
    val checklistAdded: List<Pair<Long, String>> = emptyList(),
    /** Done items: "a:<id>" (author item) or "m:<messageId>", with the ticker. */
    val checklistDone: List<Pair<String, String>> = emptyList(),
)

internal fun ContentStateOf(p: PersistedSnapshot): uniffi.lakko_ffi_uniffi.ContentState? = try {
    if (p.pollCounts != null) {
        uniffi.lakko_ffi_uniffi.ContentState.Poll(
            p.pollCounts.map { it.toUInt() },
            p.pollVoters.toUInt(),
            when {
                p.pollClosed == "time" -> uniffi.lakko_ffi_uniffi.PollClosedState.ByTime
                p.pollClosed.startsWith("op:") -> uniffi.lakko_ffi_uniffi.PollClosedState.ByOp(p.pollClosed.removePrefix("op:").toLong())
                else -> uniffi.lakko_ffi_uniffi.PollClosedState.Open
            },
        )
    } else {
        uniffi.lakko_ffi_uniffi.ContentState.Checklist(
            p.checklistClosedBy,
            p.checklistAdded.map { uniffi.lakko_ffi_uniffi.AddedChecklistItem(it.first, it.second) },
            p.checklistDone.mapNotNull { (ref, by) ->
                val item = when {
                    ref.startsWith("a:") -> ChecklistItemRef.Author(ref.removePrefix("a:").toUInt())
                    ref.startsWith("m:") -> ChecklistItemRef.Added(ref.removePrefix("m:").toLong())
                    else -> return@mapNotNull null
                }
                java.util.Base64.getDecoder().decode(by).let { uniffi.lakko_ffi_uniffi.DoneChecklistItem(item, it) }
            },
        )
    }
} catch (_: Throwable) {
    null
}

internal fun persistSnapshot(state: uniffi.lakko_ffi_uniffi.ContentState): PersistedSnapshot = when (state) {
    is uniffi.lakko_ffi_uniffi.ContentState.Poll -> PersistedSnapshot(
        pollCounts = state.counts.map { it.toLong() },
        pollVoters = state.voters.toLong(),
        pollClosed = when (val c = state.closed) {
            is uniffi.lakko_ffi_uniffi.PollClosedState.ByOp -> "op:${c.messageId}"
            uniffi.lakko_ffi_uniffi.PollClosedState.ByTime -> "time"
            else -> "open"
        },
    )
    is uniffi.lakko_ffi_uniffi.ContentState.Checklist -> PersistedSnapshot(
        checklistClosedBy = state.closedBy,
        checklistAdded = state.added.map { it.messageId to it.text },
        checklistDone = state.done.map { d ->
            val ref = when (val i = d.item) {
                is ChecklistItemRef.Author -> "a:${i.id}"
                is ChecklistItemRef.Added -> "m:${i.messageId}"
            }
            ref to b64(d.by)
        },
    )
}

/**
 * ADR-0092 section 3.11 — what a history bundle or the vault carries for a
 * content row: the content and its folded state.
 */
internal fun contentMessageOf(msg: ChatMessage, groupId: ByteArray?, nowMs: Long): uniffi.lakko_ffi_uniffi.ContentMessage? {
    val content = msg.content ?: return null
    val state = msg.contentExtras.snapshot ?: try {
        when (content) {
            is MessageContent.Poll -> pollViewOf(msg, groupId ?: ByteArray(32), null, nowMs)?.let { v ->
                uniffi.lakko_ffi_uniffi.pollStateFromCounts(
                    v.counts.map { it.toUInt() },
                    v.voters.toUInt(),
                    if (v.closed) uniffi.lakko_ffi_uniffi.PollClosedState.ByTime else uniffi.lakko_ffi_uniffi.PollClosedState.Open,
                )
            }
            is MessageContent.Checklist -> checklistViewOf(msg)?.let { v ->
                uniffi.lakko_ffi_uniffi.ContentState.Checklist(
                    if (v.closed) msg.messageId else null,
                    emptyList(),
                    v.items.mapNotNull { i -> i.doneBy?.let { uniffi.lakko_ffi_uniffi.DoneChecklistItem(i.item, it) } },
                )
            }
            else -> null
        }
    } catch (_: Throwable) {
        null
    }
    return uniffi.lakko_ffi_uniffi.ContentMessage(content, state)
}

private fun b64(bytes: ByteArray): String = java.util.Base64.getEncoder().encodeToString(bytes)
private fun unb64(s: String): ByteArray? = try {
    java.util.Base64.getDecoder().decode(s)
} catch (_: IllegalArgumentException) {
    null
}

internal fun composeContentBytes(
    content: MessageContent,
    attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef>,
): ByteArray? = try {
    uniffi.lakko_ffi_uniffi.composeContentEnvelope(
        content,
        attachments,
        uniffi.lakko_ffi_uniffi.ContentEnvelopeParts(),
    )
} catch (_: Throwable) {
    null
}

internal fun parseContentBytes(bytes: ByteArray): MessageContent? = try {
    uniffi.lakko_ffi_uniffi.parseBodyEnvelope(bytes).content
} catch (_: Throwable) {
    null
}

internal fun persistContent(msg: ChatMessage): PersistedContent? {
    val content = msg.content ?: return null
    if (msg.deleted) return null
    val envelope = composeContentBytes(content, msg.attachments) ?: return null
    val x = msg.contentExtras
    return PersistedContent(
        envelopeB64 = b64(envelope),
        ops = x.ops.map { op ->
            PersistedContentOp(
                kind = op.kind.name,
                opId = op.opId,
                createdAtMs = op.createdAtMs,
                sender = op.sender,
                privileged = op.privileged,
                choicesB64 = op.choices?.let(::b64),
                itemId = (op.item as? ChecklistItemRef.Author)?.id?.toLong(),
                itemMessageId = (op.item as? ChecklistItemRef.Added)?.messageId,
                done = op.done,
                text = op.text,
                contentEnvelopeB64 = op.content?.let { c -> composeContentBytes(c, op.attachments)?.let(::b64) },
            )
        },
        ballots = x.ballots.map { PersistedBallot(it.messageId, it.createdAtMs, b64(it.row)) },
        ringB64 = x.ring?.let(::b64),
        ownBallotChoicesB64 = x.ownBallot?.choices?.let(::b64),
        ownBallotStatus = x.ownBallot?.status?.name,
        ownBallotSetVersion = x.ownBallot?.setVersion?.toLong(),
        snapshot = x.snapshot?.let(::persistSnapshot),
    )
}

internal fun restoreContent(p: PersistedContent): Pair<MessageContent, ContentExtras>? {
    val content = unb64(p.envelopeB64)?.let(::parseContentBytes) ?: return null
    val ops = p.ops.mapNotNull { op ->
        val kind = runCatching { ContentOpKind.valueOf(op.kind) }.getOrNull() ?: return@mapNotNull null
        val edit = op.contentEnvelopeB64?.let(::unb64)?.let { bytes ->
            try {
                uniffi.lakko_ffi_uniffi.parseBodyEnvelope(bytes)
            } catch (_: Throwable) {
                null
            }
        }
        ContentOpRecord(
            kind = kind,
            opId = op.opId,
            createdAtMs = op.createdAtMs,
            sender = op.sender,
            privileged = op.privileged,
            choices = op.choicesB64?.let(::unb64),
            item = when {
                op.itemId != null -> ChecklistItemRef.Author(op.itemId.toUInt())
                op.itemMessageId != null -> ChecklistItemRef.Added(op.itemMessageId)
                else -> null
            },
            done = op.done,
            text = op.text,
            content = edit?.content,
            attachments = edit?.attachments ?: emptyList(),
        )
    }
    val own = p.ownBallotChoicesB64?.let(::unb64)?.let { choices ->
        OwnBallot(
            choices = choices,
            status = runCatching { OwnBallotStatus.valueOf(p.ownBallotStatus ?: "") }
                .getOrDefault(OwnBallotStatus.Failed)
                // A vote that was still on its way when the app stopped goes again at next open.
                .let { if (it == OwnBallotStatus.Sending) OwnBallotStatus.Failed else it },
            setVersion = p.ownBallotSetVersion?.toUInt(),
        )
    }
    return content to ContentExtras(
        ops = ops,
        ballots = p.ballots.mapNotNull { b -> unb64(b.rowB64)?.let { BallotRecord(b.messageId, b.createdAtMs, it) } },
        ring = p.ringB64?.let(::unb64),
        ownBallot = own,
        snapshot = p.snapshot?.let(::ContentStateOf),
    )
}

// ---------------------------------------------------------------- folds

internal fun identityBytes(id: MemberIdentity?): ByteArray? = id?.let(::unb64)?.takeIf { it.size == 32 }

internal fun isoToMs(createdAt: String): Long = try {
    java.time.Instant.parse(createdAt).toEpochMilli()
} catch (_: Throwable) {
    0L
}

/** What a poll card draws, from the fold of its ops (and ballots). */
public data class PollView(
    val counts: List<Int>,
    val voters: Int,
    val ownChoices: Set<Int>,
    val closed: Boolean,
    /** The poll's own close time, when it closes automatically. */
    val closeAtMs: Long?,
    /** UX 7.5 — the own anonymous vote on its way, or failed. */
    val ownBallotStatus: OwnBallotStatus? = null,
    /** An anonymous poll whose ring could not be checked: no totals, no voting. */
    val unverifiable: Boolean = false,
)

/**
 * ADR-0092 section 3.2 / 3.6 — fold a poll row. A non-anonymous poll folds its
 * named votes (`foldPoll`); an anonymous one verifies each ballot under its ring
 * and folds the counted ballots (`foldPollBallots`). [nowMs] decides a close by
 * time. Any core failure returns an empty, open view rather than throwing.
 */
public fun pollViewOf(
    msg: ChatMessage,
    groupId: ByteArray,
    ownKey: MemberIdentity?,
    nowMs: Long,
): PollView? {
    val poll = (msg.content as? MessageContent.Poll)?.poll ?: return null
    val body = poll.body
    val author = identityBytes(msg.authorKey) ?: ByteArray(32)
    val x = msg.contentExtras
    (x.snapshot as? uniffi.lakko_ffi_uniffi.ContentState.Poll)?.let { snap ->
        if (x.ops.isEmpty() && x.ballots.isEmpty()) {
            return PollView(
                counts = snap.counts.map { it.toInt() },
                voters = snap.voters.toInt(),
                ownChoices = x.ownBallot?.choices?.map { it.toInt() and 0xff }?.toSet() ?: emptySet(),
                closed = snap.closed != uniffi.lakko_ffi_uniffi.PollClosedState.Open ||
                    (body.closeAtMs?.let { nowMs >= it } ?: false),
                closeAtMs = body.closeAtMs,
            )
        }
    }
    val closes = x.ops.filter { it.kind == ContentOpKind.PollClose }.mapNotNull { op ->
        identityBytes(op.sender)?.let { uniffi.lakko_ffi_uniffi.PollCloseRecord(op.opId, it, op.privileged) }
    }
    val tally = try {
        uniffi.lakko_ffi_uniffi.foldPoll(
            body,
            author,
            msg.messageId,
            x.ops.filter { it.kind == ContentOpKind.PollVote }.mapNotNull { op ->
                val voter = identityBytes(op.sender) ?: return@mapNotNull null
                uniffi.lakko_ffi_uniffi.PollVoteRecord(op.opId, op.createdAtMs, voter, op.choices ?: ByteArray(0))
            },
            closes,
        )
    } catch (_: Throwable) {
        null
    }
    val closed = tally?.let {
        try {
            uniffi.lakko_ffi_uniffi.pollTallyIsClosed(it, nowMs)
        } catch (_: Throwable) {
            false
        }
    } ?: (body.closeAtMs?.let { nowMs >= it } ?: false)
    val anonymous = body.anonymous
    if (anonymous == null) {
        val own = identityBytes(ownKey)
        val ownChoices = own?.let { o ->
            tally?.choicesByVoter?.firstOrNull { it.voter.contentEquals(o) }?.choices
        }
        return PollView(
            counts = tally?.counts?.map { it.toInt() } ?: List(body.options.size) { 0 },
            voters = tally?.voters?.toInt() ?: 0,
            ownChoices = ownChoices?.map { it.toInt() and 0xff }?.toSet() ?: emptySet(),
            closed = closed,
            closeAtMs = body.closeAtMs,
        )
    }
    // Anonymous: the ballots, verified under the ring this device recorded.
    val ring = x.ring?.let { r ->
        try {
            uniffi.lakko_ffi_uniffi.PollRing.decode(r)
        } catch (_: Throwable) {
            null
        }
    }
    if (ring == null) {
        return PollView(
            counts = List(body.options.size) { 0 }, voters = 0, ownChoices = emptySet(), closed = closed,
            closeAtMs = body.closeAtMs, unverifiable = true,
        )
    }
    val counted = try {
        uniffi.lakko_ffi_uniffi.PollBallotKeys(groupId, body.id, anonymous.secret).use { keys ->
            x.ballots.mapNotNull { b ->
                try {
                    val v = uniffi.lakko_ffi_uniffi.openAndVerifyPollBallot(keys, ring, b.row)
                    uniffi.lakko_ffi_uniffi.CountedPollBallot(b.messageId, b.createdAtMs, v.tag, v.choices)
                } catch (_: Throwable) {
                    null
                }
            }
        }
    } catch (_: Throwable) {
        emptyList()
    }
    val effectiveClose = tally?.closedBy
    val ballotTally = try {
        uniffi.lakko_ffi_uniffi.foldPollBallots(
            uniffi.lakko_ffi_uniffi.PollBallotRules(
                optionCount = body.options.size.toUByte(),
                multiple = body.multiple,
                closeMessageId = effectiveClose,
                closeAtMs = body.closeAtMs,
            ),
            counted,
        )
    } catch (_: Throwable) {
        null
    }
    ring.close()
    val own = x.ownBallot
    return PollView(
        counts = ballotTally?.counts?.map { it.toInt() } ?: List(body.options.size) { 0 },
        voters = ballotTally?.voters?.toInt() ?: 0,
        // The own choice of an anonymous poll lives on this device only.
        ownChoices = own?.choices?.map { it.toInt() and 0xff }?.toSet() ?: emptySet(),
        closed = closed,
        closeAtMs = body.closeAtMs,
        ownBallotStatus = own?.status,
    )
}

/** What a checklist card draws. */
public data class ChecklistView(
    val title: String,
    val othersAdd: Boolean,
    val othersMark: Boolean,
    val items: List<uniffi.lakko_ffi_uniffi.ChecklistStateItem>,
    val closed: Boolean,
)

public fun checklistViewOf(msg: ChatMessage): ChecklistView? {
    val checklist = (msg.content as? MessageContent.Checklist)?.checklist ?: return null
    val author = identityBytes(msg.authorKey) ?: ByteArray(32)
    val x = msg.contentExtras
    (x.snapshot as? uniffi.lakko_ffi_uniffi.ContentState.Checklist)?.let { snap ->
        if (x.ops.isEmpty()) {
            val items = checklist.body.items.map { ChecklistItemRef.Author(it.id) to it.text } +
                snap.added.map { ChecklistItemRef.Added(it.messageId) to it.text }
            return ChecklistView(
                title = checklist.body.title,
                othersAdd = checklist.body.othersAdd,
                othersMark = checklist.body.othersMark,
                items = items.map { (ref, text) ->
                    uniffi.lakko_ffi_uniffi.ChecklistStateItem(ref, text, null, snap.done.firstOrNull { it.item == ref }?.by)
                },
                closed = snap.closedBy != null,
            )
        }
    }
    val state = try {
        uniffi.lakko_ffi_uniffi.foldChecklist(
            checklist.body,
            author,
            msg.messageId,
            x.ops.filter { it.kind == ContentOpKind.EditChecklist }.mapNotNull { op ->
                val editor = identityBytes(op.sender) ?: return@mapNotNull null
                val body = (op.content as? MessageContent.Checklist)?.checklist?.body ?: return@mapNotNull null
                uniffi.lakko_ffi_uniffi.ChecklistEditRecord(op.opId, editor, body)
            },
            x.ops.filter { it.kind == ContentOpKind.ChecklistAdd }.mapNotNull { op ->
                val adder = identityBytes(op.sender) ?: return@mapNotNull null
                uniffi.lakko_ffi_uniffi.ChecklistAddRecord(op.opId, adder, op.privileged, op.text)
            },
            x.ops.filter { it.kind == ContentOpKind.ChecklistTick }.mapNotNull { op ->
                val ticker = identityBytes(op.sender) ?: return@mapNotNull null
                val item = op.item ?: return@mapNotNull null
                uniffi.lakko_ffi_uniffi.ChecklistTickRecord(op.opId, ticker, op.privileged, item, op.done ?: false)
            },
            x.ops.filter { it.kind == ContentOpKind.PollClose }.mapNotNull { op ->
                val closer = identityBytes(op.sender) ?: return@mapNotNull null
                uniffi.lakko_ffi_uniffi.ChecklistCloseRecord(op.opId, closer)
            },
        )
    } catch (_: Throwable) {
        null
    }
    if (state == null) {
        return ChecklistView(
            title = checklist.body.title,
            othersAdd = checklist.body.othersAdd,
            othersMark = checklist.body.othersMark,
            items = checklist.body.items.map {
                uniffi.lakko_ffi_uniffi.ChecklistStateItem(ChecklistItemRef.Author(it.id), it.text, null, null)
            },
            closed = false,
        )
    }
    return ChecklistView(state.title, state.othersAdd, state.othersMark, state.items, state.closedBy != null)
}

/** The latest article of a row: the author's last edit, else the original. */
public fun articleOf(msg: ChatMessage): Pair<uniffi.lakko_ffi_uniffi.Article, List<uniffi.lakko_ffi_uniffi.AttachmentRef>>? {
    val original = (msg.content as? MessageContent.Article)?.article ?: return null
    val edit = msg.contentExtras.ops
        .filter { it.kind == ContentOpKind.EditArticle && it.sender == msg.authorKey }
        .maxByOrNull { it.opId }
    val edited = (edit?.content as? MessageContent.Article)?.article
    return if (edited != null) edited to edit.attachments else original to msg.attachments
}

/**
 * The catalog key and arguments of a one-line summary of a content message, for
 * the chat list and a notification (ADR-0092 section 1: `Poll: {question}`).
 */
public fun contentSummary(content: MessageContent): Pair<String, Map<String, String>> = when (content) {
    is MessageContent.Poll -> "preview.poll" to mapOf("question" to content.poll.body.question)
    is MessageContent.Checklist -> "preview.checklist" to mapOf("title" to content.checklist.body.title)
    is MessageContent.Location -> "preview.location" to emptyMap()
    is MessageContent.ContactCard -> "preview.contact" to mapOf("name" to content.card.name)
    is MessageContent.Article -> "preview.article" to mapOf("title" to content.article.title)
}

/**
 * Whether a failed row carrying [content] can be retried as it is. An
 * anonymous poll is bound to the roster it was composed under (its ring
 * version 0 hash, ADR-0092 section 3.4); once the roster changed, receivers
 * would refuse its ring, so the retry is refused rather than sent. [ringMatches]
 * says whether the current roster still gives that hash. Every other row
 * (a location, contact card, article, public poll, checklist, text) is.
 */
internal fun retryableAsIs(content: MessageContent?, ringMatches: (expectedHash: ByteArray) -> Boolean): Boolean {
    val anon = (content as? MessageContent.Poll)?.poll?.body?.anonymous ?: return true
    return ringMatches(anon.ring0Hash)
}

/** The refusal of [retryableAsIs]: the row stays failed and says
 *  `poll.resend_refused`. */
internal fun anonymousPollResendRefused(): ChatError = ChatError(
    "anonymous poll cannot be re-sent: the group changed since it was made",
    IllegalStateException("anonymous poll roster changed"),
    userText = me.lakko.app.l10n.UiText("poll.resend_refused"),
)

internal fun MessageContent?.isAnonymousPoll(): Boolean =
    (this as? MessageContent.Poll)?.poll?.body?.anonymous != null

/** The distinct account keys of the group's roster, sorted bytewise. */
internal fun rosterAccountKeysOf(group: uniffi.lakko_ffi_uniffi.Group): List<ByteArray> = try {
    rosterOf(group).map(::personKeyOf).distinctBy { it.toList() }.sortedWith(::compareBytes)
} catch (_: Throwable) {
    emptyList()
}

internal fun compareBytes(a: ByteArray, b: ByteArray): Int {
    for (i in 0 until minOf(a.size, b.size)) {
        val d = (a[i].toInt() and 0xff) - (b[i].toInt() and 0xff)
        if (d != 0) return d
    }
    return a.size - b.size
}

internal fun isPollBallotRowSafe(row: ByteArray): Boolean = try {
    uniffi.lakko_ffi_uniffi.isPollBallotRow(row)
} catch (_: Throwable) {
    false
}

/** A ring of [keys] encoded, when its version-0 hash is [expectedHash]. */
internal fun ringBytesIfMatches(keys: List<ByteArray>, expectedHash: ByteArray): ByteArray? = try {
    uniffi.lakko_ffi_uniffi.PollRing(keys).use { ring ->
        val h = ring.versionHash(0u)
        if (h != null && h.contentEquals(expectedHash)) ring.encode() else null
    }
} catch (_: Throwable) {
    null
}

/** Extend an encoded ring with the accounts of [rosterKeys] it does not hold. */
internal fun extendRing(ringBytes: ByteArray, rosterKeys: List<ByteArray>): ByteArray? = try {
    uniffi.lakko_ffi_uniffi.PollRing.decode(ringBytes).use { ring ->
        val added = rosterKeys.filter { !ring.contains(it) }.sortedWith(::compareBytes)
        if (added.isEmpty()) null else if (ring.extend(added) != null) ring.encode() else null
    }
} catch (_: Throwable) {
    null
}

/**
 * ADR-0092 section 3.6 — an anonymous ballot leaves the device after a uniform
 * random wait of 0..=10 s, so the moment it arrives says less about who cast
 * it. [random] is replaceable for tests.
 */
public object BallotSchedule {
    public const val MAX_DELAY_MS: Long = 10_000L
    internal var random: (Long) -> Long = { bound -> java.security.SecureRandom().nextLong().mod(bound) }
    public fun delayMs(): Long = random(MAX_DELAY_MS + 1)
}

internal fun isOpenItemsOrBallotsEnvelope(bytes: ByteArray): Boolean = try {
    uniffi.lakko_ffi_uniffi.isOpenItemsEnvelope(bytes) || uniffi.lakko_ffi_uniffi.isPollBallotsEnvelope(bytes)
} catch (_: Throwable) {
    false
}

/** How many received open-items rows a group keeps while it assembles them. Chosen. */
internal const val MAX_HELD_OPEN_ITEMS_ROWS: Int = 512

/** A received [uniffi.lakko_ffi_uniffi.ContentMessage] (history bundle, vault) as stored. */
internal fun persistedContentOf(
    c: uniffi.lakko_ffi_uniffi.ContentMessage,
    attachments: List<uniffi.lakko_ffi_uniffi.AttachmentRef>,
): PersistedContent? {
    // An article's images are the row's attachments: composed with them, so an
    // article restored from a history bundle keeps its pictures.
    val envelope = composeContentBytes(c.content, attachments) ?: return null
    return PersistedContent(
        envelopeB64 = b64(envelope),
        snapshot = c.state?.let(::persistSnapshot),
    )
}

/** [contentMessageOf] for a stored row, for the history bundle. */
public fun historyContentOf(m: PersistedMessage): uniffi.lakko_ffi_uniffi.ContentMessage? =
    m.toChatMessage()?.let { contentMessageOf(it, null, System.currentTimeMillis()) }
