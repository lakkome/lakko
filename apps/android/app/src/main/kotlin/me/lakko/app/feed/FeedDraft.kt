// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.feed

import me.lakko.app.l10n.Strings
import me.lakko.app.l10n.UiText
import me.lakko.app.l10n.joinList
import uniffi.lakko_ffi_uniffi.FeedPreset
import uniffi.lakko_ffi_uniffi.FeedRule

/**
 * A feed rule as the editor holds it (`docs/ux/feed.md` section 4.1). Mirror of
 * `apps/web/src/feed/feed-draft.ts`.
 *
 * On Android the UniFFI `FeedRule` is already a plain data class, so the
 * editor holds it directly and edits it with `copy`. The rule itself — its
 * bounds, its codec, what "valid" means — is `lakko-feed` in the core; this
 * file only turns the core's frozen refusal codes into sentences and builds
 * the sharing forms. It decides no bound of its own.
 *
 * Conversation ids are the 32-byte group ids and 16-byte channel ids as
 * lowercase hex, the same keys the feed's sources use.
 */

/** The name a new blank feed starts with: the catalog's, in the language at
 *  the moment the feed is made (it is stored as the feed's own name). */
public val BLANK_FEED_NAME: UiText = UiText("feed.editor.default_name")

/** A blank feed: "Latest", named. */
public fun blankDraft(core: FeedCore, name: String = BLANK_FEED_NAME.render()): FeedRule =
    core.preset(FeedPreset.LATEST).copy(name = name)

/** A preset as a starting point for a new feed, keeping [name]. */
public fun presetDraft(core: FeedCore, preset: FeedPreset, name: String): FeedRule =
    core.preset(preset).copy(name = name)

/** The editor's line for a refusal code. */
public fun problemText(code: String?): UiText = UiText(
    when (code) {
        "name" -> "feed.problem.name"
        "noKinds" -> "feed.problem.no_kinds"
        "weightOutOfRange" -> "feed.problem.weight"
        "windowOutOfRange" -> "feed.problem.window"
        "conversations" -> "feed.problem.conversations"
        "strippedWithSources" -> "feed.problem.stripped"
        "words" -> "feed.problem.words"
        "tooLarge" -> "feed.problem.too_large"
        else -> "feed.problem.generic"
    },
)

/** Why the draft cannot be saved, in one sentence, or `null` when it can. */
public fun draftProblem(core: FeedCore, draft: FeedRule): UiText? = try {
    core.validate(draft)
    null
} catch (e: FeedRuleRefusal) {
    problemText(e.code)
}

/** One word per line, each through the core's word rule; the ones it refuses
 *  and repeats are dropped. */
public fun parseWords(core: FeedCore, text: String): List<String> {
    val out = ArrayList<String>()
    for (line in text.split('\n')) {
        val w = core.normalizeWord(line) ?: continue
        if (w !in out) out += w
    }
    return out
}

/** Hex ids of a rule's include / exclude lists. */
public fun idsHex(ids: List<ByteArray>): List<String> = ids.map(::bytesToHex)

/** "Only" / "Never" / neither for one conversation (the editor's menu). */
public enum class Membership { ANY, ONLY, NEVER }

public fun membershipOf(draft: FeedRule, idHex: String): Membership = when {
    idsHex(draft.include).contains(idHex) -> Membership.ONLY
    idsHex(draft.exclude).contains(idHex) -> Membership.NEVER
    else -> Membership.ANY
}

/** Set one conversation's membership. Naming a conversation makes this the
 *  person's own list again, so `sourcesStripped` clears. */
public fun withMembership(draft: FeedRule, idHex: String, value: Membership): FeedRule {
    val include = draft.include.filter { bytesToHex(it) != idHex }.toMutableList()
    val exclude = draft.exclude.filter { bytesToHex(it) != idHex }.toMutableList()
    val bytes = hexToBytesOrEmpty(idHex)
    if (value == Membership.ONLY) include += bytes
    if (value == Membership.NEVER) exclude += bytes
    return draft.copy(include = include, exclude = exclude, sourcesStripped = false)
}

// ----- Sharing (UX spec section 4.1) --------------------------------------

/** The shareable bytes: the rule with its conversations stripped, since they
 *  mean nothing to anyone else and naming them would tell what the sharer is
 *  in. Throws [FeedRuleRefusal]. */
public fun exportDraft(core: FeedCore, draft: FeedRule): ByteArray = core.encode(core.export(draft))

private val b64UrlEncoder = java.util.Base64.getUrlEncoder().withoutPadding()
private val b64UrlDecoder = java.util.Base64.getUrlDecoder()

/** The link that carries a shared feed in its fragment, never its path, so no
 *  server sees the rule. */
public fun shareLink(origin: String, bytes: ByteArray): String =
    "${origin.trimEnd('/')}/feed/import#${b64UrlEncoder.encodeToString(bytes)}"

private val FRAGMENT_RE = Regex("^[A-Za-z0-9_-]+$")

/** The bytes a share link's fragment carries, or `null` when there is none. */
public fun bytesFromFragment(fragment: String?): ByteArray? {
    val raw = fragment?.removePrefix("#") ?: return null
    if (raw.isEmpty() || !FRAGMENT_RE.matches(raw)) return null
    return try {
        b64UrlDecoder.decode(raw)
    } catch (_: IllegalArgumentException) {
        null
    }
}

/** The file name a shared feed saves as. */
public fun shareFileName(draft: FeedRule): String {
    val base = draft.name
        .filter { it.isLetterOrDigit() || it == ' ' || it == '_' || it == '-' }
        .trim()
        .replace(Regex("\\s+"), "-")
    return "${base.ifEmpty { "feed" }}.lakkofeed"
}

/** The importer's preview of a shared feed's rules (UX spec section 4.1). */
public fun draftSummary(draft: FeedRule, strings: Strings): List<String> {
    val lines = ArrayList<String>()
    lines += strings[
        when {
            draft.groups && draft.channels -> "feed.summary.sources.both"
            draft.groups -> "feed.summary.sources.groups"
            else -> "feed.summary.sources.channels"
        },
    ]
    if (draft.includeMuted) lines += strings["feed.summary.include_muted"]
    val w = draft.weights
    val ranked = listOf(
        "feed.summary.factor.reactions" to w.reactions,
        "feed.summary.factor.replies" to w.replies,
        "feed.summary.factor.contacts" to w.fromContacts,
        "feed.summary.factor.mentions" to w.mentionsMe,
        "feed.summary.factor.media" to w.hasMedia,
    ).filter { it.second.toInt() > 0 }.map { strings[it.first] }
    lines += if (ranked.isEmpty()) {
        strings["feed.summary.newest_first"]
    } else {
        strings.format("feed.summary.ranks_by", mapOf("factors" to joinList(ranked, strings)))
    }
    lines += strings.format("feed.summary.look_back", counts = mapOf("count" to draft.windowDays.toULong()))
    if (draft.forMe) lines += strings["feed.summary.for_me"]
    if (draft.mediaOnly) lines += strings["feed.editor.media_only"]
    if (draft.minReactions.toInt() > 0) {
        lines += strings.format("feed.min_reactions_count", counts = mapOf("count" to draft.minReactions.toULong()))
    }
    if (draft.hideOwn) lines += strings["feed.summary.hide_own"]
    if (draft.hideWords.isNotEmpty()) {
        lines += strings.format("feed.summary.hide_words", counts = mapOf("count" to draft.hideWords.size.toULong()))
    }
    if (draft.onlyWords.isNotEmpty()) {
        lines += strings.format("feed.summary.only_words", mapOf("words" to joinList(draft.onlyWords, strings)))
    }
    return lines
}

/** The copy an import shows for anything the core refuses. */
public val FEED_IMPORT_REFUSED: UiText = UiText("feed.import.refused")
