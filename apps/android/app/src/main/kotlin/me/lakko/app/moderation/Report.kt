// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.moderation

import kotlin.coroutines.cancellation.CancellationException
import me.lakko.app.anon.AnonTokenException
import me.lakko.app.anon.AnonTokenQuotaExhausted

/*
 * ADR-0099 section 2, UX `reporting-and-moderation.md` section 2 — the report
 * sheet's rules and the send.
 *
 * Everything here is pure or runs behind seams, so the host suite can assert
 * every state of section 2.3: the core seals the report (`sealModerationReport`),
 * the moderation service receives it, and this file decides only what the
 * sheet shows and which refusal is which.
 */

/** UX section 2.2: up to 20 messages can be picked (chosen, not measured). */
public const val REPORT_MESSAGE_LIMIT: Int = 20

/** UX section 2.2: the comment is up to 500 characters (chosen). */
public const val REPORT_COMMENT_LIMIT: Int = 500

/** The report's target kind (the wire bytes of `sealModerationReport`). */
public enum class ReportTargetKind(public val wire: UByte) {
    PERSON(1u),
    GROUP(2u),
    CHANNEL(3u),
}

/** Where the report was made from (the wire bytes of `sealModerationReport`). */
public enum class ReportConversationKind(public val wire: UByte) {
    ONE_TO_ONE(1u),
    GROUP(2u),
    CHANNEL(3u),
    CONTACT_REQUEST(4u),
    DISCOVER(5u),
    CARD(6u),
}

/** What the sheet offers to do beside the report (UX section 2.2). */
public sealed interface ReportAlso {
    /** `Also block {name}`, on by default. */
    public data class Block(val accountKeyB64: String) : ReportAlso

    /** `Leave the group`, off by default. */
    public data class Leave(val groupIdHex: String) : ReportAlso

    /** `Unsubscribe`, off by default. */
    public data class Unsubscribe(val channelIdHex: String) : ReportAlso

    public data object None : ReportAlso
}

/** One message a report can include, as the device holds it. */
public data class ReportableMessage(
    val messageId: Long,
    val groupIdHex: String,
    /** `null` once the stored bytes are gone: left out of the report. */
    val evidence: MessageEvidence?,
)

/**
 * One channel post a report can include, as the device holds it: [wire] is
 * `null` once the post's stored bytes are gone, and such a post is left out.
 */
public class ReportablePost(
    public val cursor: Long,
    public val serverTsMs: Long,
    public val text: String,
    public val wire: ByteArray?,
) {
    override fun equals(other: Any?): Boolean =
        other is ReportablePost && cursor == other.cursor && serverTsMs == other.serverTsMs &&
            text == other.text && (wire?.contentEquals(other.wire) ?: (other.wire == null))

    override fun hashCode(): Int = cursor.hashCode() * 31 + text.hashCode()
}

/**
 * UX section 2.1 — the posts a channel-info report can pick from: the
 * channel's posts in the local post log (the one the feed reads), newest
 * first. A post's wire is the one the log kept, else the one of the same
 * post loaded in memory ([inMemoryWireB64] by cursor), else none.
 */
public fun channelPostCandidates(
    logged: List<me.lakko.app.feed.PersistedChannelPost>,
    inMemoryWireB64: Map<Long, String>,
): List<ReportablePost> = logged
    .distinctBy { it.cursor }
    .sortedByDescending { it.cursor }
    .map { p ->
        ReportablePost(
            cursor = p.cursor,
            serverTsMs = p.serverTsMs,
            text = p.text,
            wire = (p.wireB64 ?: inMemoryWireB64[p.cursor])?.let(::decodePostWire),
        )
    }

/** A post wire as served: base64url without padding, or standard base64. */
internal fun decodePostWire(b64: String): ByteArray? =
    runCatching { java.util.Base64.getUrlDecoder().decode(b64.trimEnd('=')) }
        .recoverCatching { java.util.Base64.getDecoder().decode(b64) }
        .getOrNull()
        ?.takeIf { it.isNotEmpty() }

/** A piece of evidence that is not a conversation message. */
public sealed interface ReportOtherEvidence {
    public class ContactRequest(public val wire: ByteArray) : ReportOtherEvidence
    public class ChannelPost(public val channelId: ByteArray, public val post: ByteArray) : ReportOtherEvidence
    public class Listing(public val wire: ByteArray) : ReportOtherEvidence
}

/**
 * Who or what is reported, from which entry point (UX section 2.1).
 * [pickAuthorKeyB64] limits what can be picked: a person's own messages only
 * (a person or a message target), or `null` for any message but the
 * reporter's own (a group or a channel target).
 */
public data class ReportSubject(
    val target: ReportTargetRef,
    val name: String,
    val conversationKind: ReportConversationKind,
    /** The conversation `Add messages` picks from, or `null` when nothing can be picked. */
    val pickGroupIdHex: String?,
    val pickAuthorKeyB64: String?,
    /** The messages included from the start (the message a menu was opened on). */
    val initialMessages: List<ReportableMessage>,
    /** A channel's info: `Add messages` picks from this channel's stored posts. */
    val pickChannelIdHex: String? = null,
    val other: List<ReportOtherEvidence>,
    val also: ReportAlso,
) {
    public val targetKind: ReportTargetKind get() = target.kind
}

/** What the report names (ADR-0099 section 2): the bytes are derived when sending. */
public sealed interface ReportTargetRef {
    public val kind: ReportTargetKind

    /** A person, by account signature key (standard base64). */
    public data class Account(val accountKeyB64: String) : ReportTargetRef {
        override val kind: ReportTargetKind get() = ReportTargetKind.PERSON
    }

    /** A group, by its `@name` (without the `@`): reported only by name. */
    public data class GroupHandle(val handle: String) : ReportTargetRef {
        override val kind: ReportTargetKind get() = ReportTargetKind.GROUP
    }

    /** A channel, by its 16-byte id (hex). */
    public data class Channel(val channelIdHex: String) : ReportTargetRef {
        override val kind: ReportTargetKind get() = ReportTargetKind.CHANNEL
    }
}

/** The send's phase, one per row of UX section 2.3. */
public enum class ReportPhase { EDITING, SENDING, FAILED, LIMITED, UNAVAILABLE }

/** The sheet's state. Immutable; every change returns a new value. */
public data class ReportSheetState(
    val reason: UByte? = null,
    val comment: String = "",
    val picked: List<ReportableMessage> = emptyList(),
    /** Posts picked from a channel's stored posts (channel-info reports). */
    val pickedPosts: List<ReportablePost> = emptyList(),
    val also: Boolean = false,
    val phase: ReportPhase = ReportPhase.EDITING,
) {
    /**
     * Send report needs a reason, accepts no second tap while sending, and
     * stays off once the server said reporting is not available here (UX
     * section 2.3: said when the sheet opens, with `Send report` disabled).
     */
    public val canSend: Boolean
        get() = reason != null && phase != ReportPhase.SENDING && phase != ReportPhase.UNAVAILABLE

    public fun withReason(code: UByte): ReportSheetState = copy(reason = code, phase = editingAfterRefusal())

    /** The comment, cut at [REPORT_COMMENT_LIMIT] characters (code points). */
    public fun withComment(text: String): ReportSheetState {
        val cut = if (text.codePointCount(0, text.length) > REPORT_COMMENT_LIMIT) {
            text.substring(0, text.offsetByCodePoints(0, REPORT_COMMENT_LIMIT))
        } else {
            text
        }
        return copy(comment = cut)
    }

    public fun withAlso(on: Boolean): ReportSheetState = copy(also = on)

    /** The picked set after the select mode's Done. Capped at [REPORT_MESSAGE_LIMIT]. */
    public fun withPicked(messages: List<ReportableMessage>): ReportSheetState =
        copy(picked = messages.distinctBy { it.messageId }.take(REPORT_MESSAGE_LIMIT))

    /** The picked posts after the select mode's Done. Capped at [REPORT_MESSAGE_LIMIT]. */
    public fun withPickedPosts(posts: List<ReportablePost>): ReportSheetState =
        copy(pickedPosts = posts.distinctBy { it.cursor }.take(REPORT_MESSAGE_LIMIT))

    /** A refusal line goes with an edit, except "not available", which no edit changes. */
    private fun editingAfterRefusal(): ReportPhase =
        if (phase == ReportPhase.SENDING || phase == ReportPhase.UNAVAILABLE) phase else ReportPhase.EDITING

    public companion object {
        public fun initial(subject: ReportSubject): ReportSheetState = ReportSheetState(
            picked = subject.initialMessages.take(REPORT_MESSAGE_LIMIT),
            // `Also block` is on by default; leaving and unsubscribing are off.
            also = subject.also is ReportAlso.Block,
        )
    }
}

/** The "Included" line (UX section 2.2 / 2.3): which catalog key, with which numbers. */
public data class IncludedLine(val key: String, val n: Int, val total: Int)

/**
 * What the "Included" line says for [picked] messages and [other] evidence:
 * `report.included` with the count, `report.included_partial` when a picked
 * message's bytes are gone, `report.included_none` when nothing is included.
 */
public fun includedLine(
    picked: List<ReportableMessage>,
    otherCount: Int,
    pickedPosts: List<ReportablePost> = emptyList(),
): IncludedLine {
    val total = picked.size + otherCount + pickedPosts.size
    val usable = picked.count { it.evidence != null } + otherCount + pickedPosts.count { it.wire != null }
    return when {
        total == 0 -> IncludedLine("report.included_none", 0, 0)
        usable < total -> IncludedLine("report.included_partial", usable, total)
        else -> IncludedLine("report.included", usable, total)
    }
}

/**
 * Whether [authorKeyB64]'s message with [isOwn] may be picked for [subject]:
 * own messages never; a person target only that person's messages; a group
 * or channel target any other message.
 */
public fun pickable(subject: ReportSubject, authorKeyB64: String?, isOwn: Boolean): Boolean {
    if (isOwn || authorKeyB64 == null) return false
    val only = subject.pickAuthorKeyB64 ?: return true
    return canonicalKey(only) == canonicalKey(authorKeyB64)
}

/** The highest moderator-list version this device saw is kept; a lower one is unavailable. */
public fun moderatorListAccepted(version: ULong, highestSeen: ULong?): Boolean =
    highestSeen == null || version >= highestSeen

// -----------------------------------------------------------------------------
// The send
// -----------------------------------------------------------------------------

/** How a send ended. */
public sealed interface ReportOutcome {
    public data class Sent(val receiptId: String) : ReportOutcome

    /** `report.failed`: no network, or anything that is not one of the others. */
    public data object Failed : ReportOutcome

    /** `report.limited`: the daily token allowance or the service's rate limit. */
    public data object Limited : ReportOutcome

    /** `report.unavailable`: no pinned root key, no moderator list, or an older one. */
    public data object Unavailable : ReportOutcome
}

/** The moderator-list fetch. */
public sealed interface ModeratorListFetch {
    public class Ok(public val bytes: ByteArray) : ModeratorListFetch
    public data object Unavailable : ModeratorListFetch
    public data object Network : ModeratorListFetch
}

/** The report POST. */
public sealed interface ReportPost {
    public data class Created(val receiptId: String) : ReportPost
    public data class Refused(val status: Int, val error: String?) : ReportPost
    public data object Network : ReportPost
}

/** The seams of [sendReport]. */
public interface ReportPorts {
    /** The pinned root key (32 bytes), or `null` when the build pins none. */
    public fun rootKey(): ByteArray?
    public suspend fun fetchModeratorList(): ModeratorListFetch

    /** The list's version under [rootKey]; throws when it does not verify. */
    public fun moderatorListVersion(list: ByteArray, rootKey: ByteArray): ULong
    public fun highestListVersion(): ULong?
    public fun recordListVersion(version: ULong)

    /** One `report` token's header value; throws the pool's exceptions. */
    public suspend fun takeToken(): String

    /** Seal the report under the verified list; throws on any refusal. */
    public suspend fun seal(list: ByteArray, rootKey: ByteArray): ByteArray
    public suspend fun post(report: ByteArray, tokenHeaderValue: String): ReportPost
}

/** Token refusals that a fresh token can fix (`ANON_TOKEN_RETRYABLE_REFUSALS`). */
private fun retryableTokenRefusal(post: ReportPost): Boolean =
    post is ReportPost.Refused && post.status == 401 &&
        post.error in setOf("anon-token-spent", "anon-token-expired", "anon-token-wrong-context", "anon-token-malformed")

/** The pinned root key and a moderator list it accepts, or why not. */
private sealed interface ListStep {
    class Ok(val root: ByteArray, val list: ByteArray) : ListStep
    data object Unavailable : ListStep
    data object Network : ListStep
}

private suspend fun verifiedModeratorList(ports: ReportPorts): ListStep {
    val root = ports.rootKey() ?: return ListStep.Unavailable
    val list = when (val fetched = ports.fetchModeratorList()) {
        is ModeratorListFetch.Ok -> fetched.bytes
        ModeratorListFetch.Unavailable -> return ListStep.Unavailable
        ModeratorListFetch.Network -> return ListStep.Network
    }
    val version = try {
        ports.moderatorListVersion(list, root)
    } catch (cause: CancellationException) {
        throw cause
    } catch (_: Throwable) {
        return ListStep.Unavailable
    }
    if (!moderatorListAccepted(version, ports.highestListVersion())) return ListStep.Unavailable
    ports.recordListVersion(version)
    return ListStep.Ok(root, list)
}

/** Whether reporting can work on this server, asked when the sheet opens. */
public enum class ReportAvailability {
    AVAILABLE,

    /** No pinned root key, the service answers 503, or the list is refused. */
    UNAVAILABLE,

    /** No answer (no network): the sheet stays as it is and a send says what happened. */
    UNKNOWN,
}

/**
 * UX section 2.3, "Reporting not available on this server": said when the
 * sheet opens, and `Send report` is disabled, so the sheet's block or leave
 * is never offered against a report that cannot go. Seals and posts nothing.
 * Never throws but for cancellation.
 */
public suspend fun checkReportAvailability(ports: ReportPorts): ReportAvailability = try {
    when (verifiedModeratorList(ports)) {
        is ListStep.Ok -> ReportAvailability.AVAILABLE
        ListStep.Unavailable -> ReportAvailability.UNAVAILABLE
        ListStep.Network -> ReportAvailability.UNKNOWN
    }
} catch (cause: CancellationException) {
    throw cause
} catch (_: Throwable) {
    ReportAvailability.UNKNOWN
}

/**
 * Send one report (UX section 2.3). Never throws but for cancellation.
 * The moderator list is fetched, verified under the pinned root and checked
 * against the highest version seen, before anything is sealed.
 */
public suspend fun sendReport(ports: ReportPorts): ReportOutcome {
    try {
        val (root, list) = when (val step = verifiedModeratorList(ports)) {
            is ListStep.Ok -> step.root to step.list
            ListStep.Unavailable -> return ReportOutcome.Unavailable
            ListStep.Network -> return ReportOutcome.Failed
        }
        val sealed = ports.seal(list, root)
        var post = ports.post(sealed, ports.takeToken())
        if (retryableTokenRefusal(post)) post = ports.post(sealed, ports.takeToken())
        return when (post) {
            is ReportPost.Created -> ReportOutcome.Sent(post.receiptId)
            is ReportPost.Refused -> when {
                post.status == 429 -> ReportOutcome.Limited
                post.status == 503 -> ReportOutcome.Unavailable
                else -> ReportOutcome.Failed
            }
            ReportPost.Network -> ReportOutcome.Failed
        }
    } catch (cause: CancellationException) {
        throw cause
    } catch (cause: AnonTokenQuotaExhausted) {
        return ReportOutcome.Limited
    } catch (cause: AnonTokenException) {
        return ReportOutcome.Failed
    } catch (_: Throwable) {
        return ReportOutcome.Failed
    }
}

/** The phase a sheet shows after [outcome]; `null` after a send (the sheet closes). */
public fun phaseAfter(outcome: ReportOutcome): ReportPhase? = when (outcome) {
    is ReportOutcome.Sent -> null
    ReportOutcome.Failed -> ReportPhase.FAILED
    ReportOutcome.Limited -> ReportPhase.LIMITED
    ReportOutcome.Unavailable -> ReportPhase.UNAVAILABLE
}

/**
 * Whether the sheet's `Also` action runs after [outcome] (UX section 2.3):
 * the block, leave or unsubscribe is applied on a send, and a block still
 * applies when the limit is reached or reporting is not available.
 */
public fun alsoApplies(outcome: ReportOutcome, also: ReportAlso): Boolean = when (outcome) {
    is ReportOutcome.Sent -> true
    ReportOutcome.Limited, ReportOutcome.Unavailable -> also is ReportAlso.Block
    ReportOutcome.Failed -> false
}

/** The catalog key of a reason byte (`report.reason.child_safety`), or `null`. */
public fun reasonKey(name: String?): String? = name?.let { "report.reason." + it.replace('-', '_') }
