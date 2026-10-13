// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import uniffi.lakko_ffi_uniffi.FeedCandidate
import uniffi.lakko_ffi_uniffi.FeedConversation
import uniffi.lakko_ffi_uniffi.FeedConversationKind
import uniffi.lakko_ffi_uniffi.FeedEntry
import uniffi.lakko_ffi_uniffi.FeedPreset
import uniffi.lakko_ffi_uniffi.FeedRule
import uniffi.lakko_ffi_uniffi.FfiException

/**
 * The one ranking and the one rule, reached through the UniFFI core
 * (ADR-0063). Mirror of `apps/web/src/feed/feed-ranking.ts`.
 *
 * The order lives in `lakko-feed` so web, iOS and Android draw the same
 * messages in the same order; this file only moves [buildFeedInput]'s lists
 * across the boundary and maps the returned indices back to rows. It re-derives
 * nothing: a filter or a tie-break written here would be the drift the core
 * exists to prevent.
 *
 * [FeedCore] is an interface so the host JVM suite, which has no native
 * library, can drive the code around the core with a fake.
 */
public interface FeedCore {
    public fun rank(
        rule: FeedRule,
        conversations: List<FeedConversation>,
        candidates: List<FeedCandidate>,
        nowMs: Long,
    ): List<FeedEntry>

    public fun mentionsUsername(text: String, username: String): Boolean

    /** The highest id with every listed id at or below it seen, or `null`. */
    public fun contiguousSeen(ids: List<Long>, seen: List<Long>): Long?

    /** Throws [FeedRuleRefusal] when the rule breaks a bound. */
    public fun validate(rule: FeedRule)

    /** Canonical bytes; throws [FeedRuleRefusal]. */
    public fun encode(rule: FeedRule): ByteArray

    /** Throws [FeedRuleRefusal] on anything the encoder would not produce. */
    public fun decode(bytes: ByteArray): FeedRule

    /** The rule with its conversations stripped, for sharing. */
    public fun export(rule: FeedRule): FeedRule

    public fun preset(preset: FeedPreset): FeedRule

    public fun normalizeWord(raw: String): String?

    public fun maxWeight(): Int

    public fun ruleMaxLen(): Int
}

/** The core refused a rule; [code] is the frozen reason code (`name`,
 *  `noKinds`, ...), `null` when the message carried none. */
public class FeedRuleRefusal(public val code: String?, message: String) : Exception(message)

/** The frozen code at the head of a core refusal message `"<code>: <reason>"`. */
public fun refusalCodeOf(message: String?): String? {
    if (message.isNullOrEmpty()) return null
    val head = message.substringBefore(':', missingDelimiterValue = "").trim()
    return head.takeIf { it.isNotEmpty() && it.all(Char::isLetterOrDigit) }
}

/** The production [FeedCore]: the UniFFI functions. */
public object UniffiFeedCore : FeedCore {
    private inline fun <T> refusing(block: () -> T): T = try {
        block()
    } catch (e: FfiException.FeedRule) {
        throw FeedRuleRefusal(refusalCodeOf(e.message), e.message ?: "refused")
    }

    override fun rank(
        rule: FeedRule,
        conversations: List<FeedConversation>,
        candidates: List<FeedCandidate>,
        nowMs: Long,
    ): List<FeedEntry> = uniffi.lakko_ffi_uniffi.rankFeed(rule, conversations, candidates, nowMs)

    override fun mentionsUsername(text: String, username: String): Boolean =
        uniffi.lakko_ffi_uniffi.mentionsUsername(text, username)

    override fun contiguousSeen(ids: List<Long>, seen: List<Long>): Long? =
        uniffi.lakko_ffi_uniffi.contiguousSeen(ids, seen)

    override fun validate(rule: FeedRule): Unit = refusing { uniffi.lakko_ffi_uniffi.validateFeedRule(rule) }

    override fun encode(rule: FeedRule): ByteArray = refusing { uniffi.lakko_ffi_uniffi.encodeFeedRule(rule) }

    override fun decode(bytes: ByteArray): FeedRule = refusing { uniffi.lakko_ffi_uniffi.decodeFeedRule(bytes) }

    override fun export(rule: FeedRule): FeedRule = uniffi.lakko_ffi_uniffi.exportFeedRule(rule)

    override fun preset(preset: FeedPreset): FeedRule = uniffi.lakko_ffi_uniffi.feedPresetRule(preset)

    override fun normalizeWord(raw: String): String? = uniffi.lakko_ffi_uniffi.normalizeFeedWord(raw)

    override fun maxWeight(): Int = uniffi.lakko_ffi_uniffi.feedMaxWeight().toInt()

    override fun ruleMaxLen(): Int = uniffi.lakko_ffi_uniffi.feedRuleMaxLen().toInt()
}

/** One item to draw: a message and the replies folded under it. */
public data class RankedItem(
    val ref: FeedCandidateRef,
    val replies: List<FeedCandidateRef>,
) {
    /** Stable identity across re-ranks. */
    val key: String get() = itemKey(ref)
}

/** `group:<hex>:<id>` / `channel:<id>:<cursor>`. */
public fun itemKey(ref: FeedCandidateRef): String =
    "${ref.source.kind.name.lowercase()}:${ref.source.idHex}:${ref.message.messageId}"

internal fun hexToBytesOrEmpty(hex: String): ByteArray {
    if (hex.length % 2 != 0) return ByteArray(0)
    val out = ByteArray(hex.length / 2)
    for (i in out.indices) {
        val hi = Character.digit(hex[i * 2], 16)
        val lo = Character.digit(hex[i * 2 + 1], 16)
        if (hi < 0 || lo < 0) return ByteArray(0)
        out[i] = ((hi shl 4) or lo).toByte()
    }
    return out
}

internal fun bytesToHex(bytes: ByteArray): String =
    bytes.joinToString("") { "%02x".format(it.toInt() and 0xff) }

/** Rank [input] under [rule] at [nowMs] and map the entries back to rows. */
public fun rankFeedInput(core: FeedCore, rule: FeedRule, input: FeedInput, nowMs: Long): List<RankedItem> {
    val conversations = input.sources.map { s ->
        FeedConversation(
            // ADR-0073 R.5 — a rule's include / exclude lists name
            // conversation ids.
            id = hexToBytesOrEmpty(feedSourceKey(s)),
            kind = if (s.kind == FeedSourceKind.CHANNEL) FeedConversationKind.CHANNEL else FeedConversationKind.GROUP,
            muted = s.muted,
            audience = s.audience.coerceAtLeast(0).toUInt(),
        )
    }
    val candidates = input.candidates.map { c ->
        FeedCandidate(
            conversation = c.conversation.toUInt(),
            messageId = c.messageId,
            replyTo = c.replyTo,
            timeMs = c.timeMs,
            fromSelf = c.fromSelf,
            fromContact = c.fromContact,
            mentionsMe = c.mentionsMe,
            repliesToMe = c.repliesToMe,
            reactionsDistinct = c.reactionsDistinct.coerceAtLeast(0).toUInt(),
            hasMedia = c.hasMedia,
            text = c.text,
        )
    }
    val entries = core.rank(rule, conversations, candidates, nowMs)
    val out = ArrayList<RankedItem>(entries.size)
    for (e in entries) {
        val ref = input.refs.getOrNull(e.index.toInt()) ?: continue
        val replies = e.replies.mapNotNull { input.refs.getOrNull(it.toInt()) }
        out += RankedItem(ref, replies)
    }
    return out
}
