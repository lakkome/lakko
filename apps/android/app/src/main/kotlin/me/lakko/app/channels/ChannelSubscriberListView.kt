// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import uniffi.lakko_ffi_uniffi.ChannelSubscriberCard
import uniffi.lakko_ffi_uniffi.ChannelSubscriberList

/**
 * `docs/ux/group-and-channel-privacy.md` sections 15.1 and 7.3 — a channel's
 * subscriber list as a person sees it, built by the shared rule
 * (`lakko_ui_rules::subscribers`, `channelSubscriberList`): a card that
 * carries no name is not a named row; a person shown by name is never listed
 * again among the others; in a private channel the owner and publishers may
 * remove every row, named rows included. Base walk 2026-11-13: a nameless
 * card read "Member", and the owner could not tell whom `Remove` took.
 *
 * The rule is an FFI call the host JVM cannot load; this file holds the two
 * halves around it, which are: what is passed in, and what comes back.
 */
public object ChannelSubscriberListView {
    /** One person shown by name. */
    public data class Person(
        val row: ChannelMemberRow,
        /** Every subscriber key of this person: what `Remove` takes. */
        val pubkeysHex: List<String>,
        val removable: Boolean,
    )

    /** What the screen draws. */
    public data class View(
        val people: List<Person>,
        /** The others listed one row each (private channel, owner, publishers). */
        val others: List<ChannelSubscriberRows.Row>,
        /** The number in "{n} others."; `0` says nothing. */
        val othersCount: Long,
    )

    /** The rule's input: one card per subscriber key of each member row. */
    public class Input(
        public val cards: List<ChannelSubscriberCard>,
        internal val cardRows: List<ChannelMemberRow>,
    )

    /**
     * [ownKeyHex]: this device's own registered subscriber key; always the
     * viewer's own ("You"), card or not, so it is never an anonymous row to
     * remove.
     */
    public fun input(
        members: List<ChannelMemberRow>,
        ownKeyHex: String? = null,
        /** Keys that are not subscribers ([SubscriberCount.ownPublisherKeys]):
         *  left out of every card. */
        excludedKeys: Set<String> = emptySet(),
    ): Input {
        val own = ownKeyHex?.lowercase()?.takeUnless { it in excludedKeys }
        val notExcluded = if (excludedKeys.isEmpty()) {
            members
        } else {
            members.mapNotNull { m ->
                val kept = m.subscriberPksHex.filterNot { it.lowercase() in excludedKeys }
                if (kept.isEmpty()) null else m.copy(subscriberPksHex = kept)
            }
        }
        @Suppress("NAME_SHADOWING")
        val members = if (own != null && notExcluded.none { m -> m.subscriberPksHex.any { it.lowercase() == own } }) {
            val selfRow = notExcluded.firstOrNull { it.isSelf }
            val you = selfRow ?: ChannelMemberRow(key = "self", name = "", username = null, isSelf = true, named = true)
            listOf(you.copy(subscriberPksHex = you.subscriberPksHex + own)) + notExcluded.filter { it !== selfRow }
        } else {
            notExcluded
        }
        val cards = ArrayList<ChannelSubscriberCard>()
        val cardRows = ArrayList<ChannelMemberRow>()
        for (row in members) {
            for (pk in row.subscriberPksHex) {
                cards += ChannelSubscriberCard(
                    subscriberKey = pk.lowercase(),
                    personKey = if (row.isSelf) "self" else row.key,
                    named = row.named,
                    isSelf = row.isSelf,
                )
                cardRows += row
            }
        }
        return Input(cards, cardRows)
    }

    /** [listed] less the rows of [excludedKeys] (lower-case hex). */
    public fun withoutKeys(listed: List<ChannelSubscriberRows.Row>, excludedKeys: Set<String>): List<ChannelSubscriberRows.Row> =
        if (excludedKeys.isEmpty()) listed else listed.filterNot { it.pubkeyHex.lowercase() in excludedKeys }

    /** Map the rule's answer back onto the rows. */
    public fun view(input: Input, listed: List<ChannelSubscriberRows.Row>, out: ChannelSubscriberList): View {
        val people = out.people.mapNotNull { person ->
            val first = person.cards.firstOrNull()?.toInt() ?: return@mapNotNull null
            val row = input.cardRows.getOrNull(first) ?: return@mapNotNull null
            Person(
                row = row,
                pubkeysHex = person.cards.mapNotNull { input.cards.getOrNull(it.toInt())?.subscriberKey },
                removable = person.removable,
            )
        }
        val others = out.others.mapNotNull { listed.getOrNull(it.toInt()) }
        val count = if (out.othersCount > Long.MAX_VALUE.toULong()) Long.MAX_VALUE else out.othersCount.toLong()
        return View(people, others, count)
    }

    /** The whole list, through the shared rule. */
    public fun build(
        members: List<ChannelMemberRow>,
        listed: List<ChannelSubscriberRows.Row>,
        count: Long?,
        canRemove: Boolean,
        ownKeyHex: String? = null,
        /** Keys that are not subscribers: the publishing viewer's own
         *  ([SubscriberCount.ownPublisherKeys]). [count] must already leave
         *  them out ([SubscriberCount.shown]). */
        excludedKeys: Set<String> = emptySet(),
    ): View {
        val input = input(members, ownKeyHex, excludedKeys)
        val kept = withoutKeys(listed, excludedKeys)
        val out = uniffi.lakko_ffi_uniffi.channelSubscriberList(
            cards = input.cards,
            listedKeys = kept.map { it.pubkeyHex.lowercase() },
            count = count?.coerceAtLeast(0L)?.toULong(),
            canRemove = canRemove,
        )
        return view(input, kept, out)
    }

    /**
     * Whether a reader's own member card is due for its newest held epoch:
     * a subscriber of a private channel holds no seed until the owner's
     * device lets it in, so the card is uploaded when a seed first arrives
     * and again after each rotation, not only when the channel screen opens
     * (base walk 2026-11-13: the Pixel's card was never uploaded).
     */
    public fun readerCardDue(lastUploadedEpoch: UInt?, newestHeldEpoch: UInt?): Boolean =
        newestHeldEpoch != null && (lastUploadedEpoch == null || newestHeldEpoch > lastUploadedEpoch)
}
