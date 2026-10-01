// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

import me.lakko.app.l10n.UiText
import kotlinx.serialization.Serializable

/**
 * Channel cards (docs/ux/feed.md section 6): what one suggestion is, how the
 * three sources become cards, how this device orders them, and the check a
 * card must pass before it can be subscribed to. Mirror of
 * `apps/web/src/discover/cards.ts`.
 *
 * Ranking for the person happens here, not in the ADR-0063 engine: the engine
 * ranks messages of conversations the device holds, and a card is none of
 * those. [scoreCard] is a small local scorer over what a card carries; integer
 * arithmetic, chosen weights (not measured), identical to web.
 *
 * Pure: the listing decoder is injected (the core codec lives behind the FFI).
 */

/** The published subscriber bands, smallest first (ADR-0059 section 4). */
public val SUBSCRIBER_BANDS: List<String> = listOf("<10", "10+", "100+", "1k+", "10k+", "100k+")

/** Where a card came from. */
public enum class CardSource { BUCKET, SEARCH, GROUP }

/** People in one group who follow a channel. */
public data class GroupFollowers(
    val groupIdHex: String,
    val groupTitle: String,
    /** Distinct members of that group sharing it. */
    val count: Int,
    /** Display names of those among them who are the person's contacts. */
    val contactNames: List<String>,
)

/** One suggestion. */
public data class DirectoryCard(
    /** 32-char lowercase hex. */
    val channelIdHex: String,
    /** The claimed handle. Plain text, never a link, until verified. */
    val handle: String,
    val name: String,
    /** `null` until loaded (a group-shared card fetches it when opened). */
    val description: String?,
    /** `null` when not known (a group-shared card before it is opened). */
    val topic: Int?,
    val language: String?,
    /** One of [SUBSCRIBER_BANDS], or `null` when not known. */
    val band: String?,
    val source: CardSource,
    /** Position in the server's list (bucket or search), 0 = first. */
    val serverRank: Int,
    /** Set for a card that came from groups. */
    val followers: GroupFollowers?,
)

/** One item of a bucket or search response, as the server sends it. */
@Serializable
public data class DirectoryItemWire(
    val channel_id: String = "",
    val listing_b64: String = "",
    val subscribers: String = "",
)

/** A decoded listing (the FFI `FfiChannelListing`, structurally). */
public data class DecodedListing(
    val channelIdHex: String,
    val handle: String,
    val name: String,
    val description: String,
    val topic: Int,
    val language: String,
    val updatedAtMs: ULong = 0uL,
)

/**
 * Directory items to cards. An item whose listing does not decode, or names
 * another channel than the item's id, is dropped: a mismatch means the server
 * served something it should not have.
 */
public fun cardsFromItems(
    items: List<DirectoryItemWire>,
    source: CardSource,
    decode: (listingB64: String) -> DecodedListing?,
): List<DirectoryCard> {
    val out = mutableListOf<DirectoryCard>()
    val seen = HashSet<String>()
    items.forEachIndexed { i, item ->
        val listing = decode(item.listing_b64) ?: return@forEachIndexed
        val id = item.channel_id.lowercase()
        if (listing.channelIdHex != id || !seen.add(id)) return@forEachIndexed
        out += DirectoryCard(
            channelIdHex = id,
            handle = listing.handle,
            name = listing.name,
            description = listing.description,
            topic = listing.topic,
            language = listing.language,
            band = item.subscribers.takeIf { it in SUBSCRIBER_BANDS },
            source = source,
            serverRank = i,
            followers = null,
        )
    }
    return out
}

/** What the scorer and the filter read about the person. */
public data class CardContext(
    val pickedTopics: Set<Int>,
    val language: String,
    val notInterested: Set<String>,
    val hiddenTopics: Set<Int>,
    /** Channels this device already follows or publishes. */
    val following: Set<String>,
    /** Cards found gone this session (subscribe refused). */
    val gone: Set<String>,
)

/** Whether a card may be shown at all. */
public fun cardVisible(card: DirectoryCard, ctx: CardContext): Boolean {
    if (card.channelIdHex in ctx.notInterested) return false
    if (card.channelIdHex in ctx.following) return false
    if (card.channelIdHex in ctx.gone) return false
    if (card.topic != null && card.topic in ctx.hiddenTopics) return false
    return true
}

/** The device-side score (chosen weights). Higher is better. */
public fun scoreCard(card: DirectoryCard, ctx: CardContext): Int {
    var score = 0
    if (card.topic != null && card.topic in ctx.pickedTopics) score += 100
    if (card.language != null && card.language == ctx.language) score += 50
    card.followers?.let { score += 40 * minOf(it.count, 5) }
    val band = card.band?.let { SUBSCRIBER_BANDS.indexOf(it) } ?: -1
    if (band >= 0) score += 10 * band
    score += maxOf(0, 100 - card.serverRank)
    return score
}

/**
 * Merge cards from every source (a channel seen twice becomes one card with
 * the richest fields and the best server rank), drop the hidden ones and order
 * the rest: score, then name, then id, so the order is total.
 */
public fun rankCards(cards: List<DirectoryCard>, ctx: CardContext): List<DirectoryCard> {
    val merged = LinkedHashMap<String, DirectoryCard>()
    for (c in cards) {
        val held = merged[c.channelIdHex]
        if (held == null) {
            merged[c.channelIdHex] = c
            continue
        }
        val withListing = if (held.description != null) held else c
        val followers = held.followers ?: c.followers
        merged[c.channelIdHex] = withListing.copy(
            serverRank = minOf(held.serverRank, c.serverRank),
            followers = followers,
            source = if (held.followers != null || c.followers != null) CardSource.GROUP else withListing.source,
        )
    }
    return merged.values
        .filter { cardVisible(it, ctx) }
        .map { it to scoreCard(it, ctx) }
        .sortedWith(
            compareByDescending<Pair<DirectoryCard, Int>> { it.second }
                .thenBy { it.first.name }
                .thenBy { it.first.channelIdHex },
        )
        .map { it.first }
}

/** How this device stands to a card's channel (UX spec section 6.2). */
public enum class CardRelation { OWN, FOLLOWING, NONE }

/**
 * A channel this device can post to is its own (owner or co-publisher); one it
 * only holds in its list is followed. Hex case does not matter on either side.
 */
public fun cardRelation(
    channelIdHex: String,
    publishes: Collection<String>,
    inList: Collection<String>,
): CardRelation {
    val id = channelIdHex.lowercase()
    if (publishes.any { it.lowercase() == id }) return CardRelation.OWN
    if (inList.any { it.lowercase() == id }) return CardRelation.FOLLOWING
    return CardRelation.NONE
}

/** The reason line for a held channel, or null to use [reasonLine]. */
public fun relationLine(relation: CardRelation): UiText? = when (relation) {
    CardRelation.OWN -> UiText("channels.row.own")
    CardRelation.FOLLOWING -> UiText("discover.card.following")
    CardRelation.NONE -> null
}

/** The reason line under a card (UX spec section 6). Names only contacts. */
public fun reasonLine(card: DirectoryCard): UiText {
    val f = card.followers
    if (f != null) {
        val named = f.contactNames.firstOrNull()
        val group = mapOf("group" to f.groupTitle)
        if (named != null) {
            val others = f.count - 1
            if (others <= 0) return UiText("discover.reason.followed_by", mapOf("name" to named) + group)
            return UiText(
                "discover.reason.followed_by_others",
                mapOf("name" to named) + group,
                counts = mapOf("count" to others.toULong()),
            )
        }
        return UiText("discover.reason.followed_count", group, counts = mapOf("count" to f.count.coerceAtLeast(0).toULong()))
    }
    if (card.source == CardSource.SEARCH) return UiText("discover.reason.search")
    return UiText("discover.reason.popular", parts = mapOf("topic" to topicName(card.topic ?: 0)))
}

/** The handle check before subscribing (ADR-0059 section 3). */
public enum class VerifyDecision { OK, GONE }

/**
 * The card's handle, resolved through the normal OPRF channel lookup, must
 * name the card's channel. Anything else (no such handle, another channel) is
 * "gone".
 */
public fun verifyDecision(cardChannelIdHex: String, resolvedHex: String?): VerifyDecision {
    if (resolvedHex == null) return VerifyDecision.GONE
    return if (resolvedHex.equals(cardChannelIdHex, ignoreCase = true)) VerifyDecision.OK else VerifyDecision.GONE
}

/** How often a suggestion appears in a feed (the rule's `suggestions`). */
public enum class SuggestionRate(val every: Int) { NEVER(0), RARELY(20), SOMETIMES(8) }

/** A slot in a feed that mixes cards in. */
public sealed interface MixedSlot {
    public val index: Int

    public data class Item(override val index: Int) : MixedSlot

    public data class Card(override val index: Int) : MixedSlot
}

/**
 * Interleave up to [cardCount] cards into [itemCount] feed items at [rate]:
 * one card after every N items, never at the very top and never after the
 * last item (a feed that ends on a suggestion reads as an ad).
 */
public fun mixSuggestions(itemCount: Int, cardCount: Int, rate: SuggestionRate): List<MixedSlot> {
    val out = mutableListOf<MixedSlot>()
    val every = rate.every
    var card = 0
    for (i in 0 until itemCount) {
        out += MixedSlot.Item(i)
        val placed = i + 1
        if (every > 0 && card < cardCount && placed % every == 0 && placed < itemCount) {
            out += MixedSlot.Card(card)
            card += 1
        }
    }
    return out
}

/** Item index -> the card index shown after it. */
public fun cardsAfterItems(itemCount: Int, cardCount: Int, rate: SuggestionRate): Map<Int, Int> {
    val out = HashMap<Int, Int>()
    var last = -1
    for (slot in mixSuggestions(itemCount, cardCount, rate)) {
        when (slot) {
            is MixedSlot.Item -> last = slot.index
            is MixedSlot.Card -> out[last] = slot.index
        }
    }
    return out
}
