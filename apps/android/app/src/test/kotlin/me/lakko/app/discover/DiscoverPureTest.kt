// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import me.lakko.app.l10n.UiText

/**
 * ADR-0059 S4 Android — the pure Discover rules: decoy planning, card merge /
 * score / filter / reason line, the handle check and suggestion mixing, and the
 * prefs codec. Mirrors `apps/web/src/discover/{decoys,cards,discover-store}.test.ts`.
 */
class DiscoverPureTest {

    private val allTopics = (0 until 24).toList()
    private fun seq(vararg v: Double): () -> Double {
        var i = 0
        return { v[i++ % v.size] }
    }

    // --- decoys ------------------------------------------------------------

    @Test
    fun `the month key is UTC and zero-padded`() {
        assertEquals("2026-09", monthKey(1_790_000_000_000L))
        assertEquals("1970-01", monthKey(0L))
    }

    @Test
    fun `a held draw of this month and language is reused, anything else is redrawn`() {
        val now = 1_790_000_000_000L
        val held = DecoyDraw(monthKey(now), "en", allTopics.reversed())
        assertEquals(CurrentDecoys(held, false), currentDecoys(held, allTopics, "en", now, seq(0.5)))
        assertTrue(currentDecoys(held, allTopics, "de", now, seq(0.5)).fresh)
        assertTrue(currentDecoys(held.copy(month = "2020-01"), allTopics, "en", now, seq(0.5)).fresh)
        assertTrue(currentDecoys(null, allTopics, "en", now, seq(0.5)).fresh)
        assertEquals(allTopics.toSet(), currentDecoys(null, allTopics, "en", now, seq(0.3)).draw.order.toSet())
    }

    @Test
    fun `rounds hold every picked topic, pad to eight with unpicked decoys, and nothing without picks`() {
        val draw = DecoyDraw("2026-09", "en", allTopics)
        assertEquals(emptyList<List<Int>>(), planBucketRounds(emptyList(), draw, true, seq(0.1)))
        val rounds = planBucketRounds(listOf(3, 5, 5), draw, true, seq(0.1, 0.9))
        assertEquals(1, rounds.size)
        assertEquals(8, rounds[0].size)
        assertTrue(rounds[0].containsAll(listOf(3, 5)))
        assertEquals(8, rounds[0].toSet().size)
        val picked = (0 until 10).toList()
        val two = planBucketRounds(picked, draw, true, seq(0.2))
        assertEquals(2, two.size)
        assertTrue(two.flatten().containsAll(picked))
        two.forEach { assertEquals(8, it.size) }
        assertEquals(listOf(3), planBucketRounds(listOf(3), draw, false, seq(0.2)).single())
    }

    @Test
    fun `the same month draw gives the same decoys every day`() {
        val draw = DecoyDraw("2026-09", "en", allTopics.shuffled(java.util.Random(7)))
        val a = planBucketRounds(listOf(1, 2), draw, true, seq(0.4)).flatten().toSet()
        val b = planBucketRounds(listOf(1, 2), draw, true, seq(0.8)).flatten().toSet()
        assertEquals(a, b)
    }

    @Test
    fun `a refresh is due when never fetched, a day old, the clock ran back, or the picks changed`() {
        val k = pickedKey(listOf(2, 1, 2), "en")
        assertEquals("en:1,2", k)
        assertTrue(refreshDue(null, null, k, 10))
        assertTrue(refreshDue(0, "en:1", k, 10))
        assertFalse(refreshDue(1000, k, k, 2000))
        assertTrue(refreshDue(1000, k, k, 1000 + Decoys.REFRESH_INTERVAL_MS))
        assertTrue(refreshDue(1000, k, k, 500))
    }

    // --- cards -------------------------------------------------------------

    private fun listing(id: String, topic: Int = 1, name: String = "N$id") =
        DecodedListing(id, "h$id", name, "desc", topic, "en")

    private val ctx = CardContext(
        pickedTopics = setOf(1),
        language = "en",
        notInterested = emptySet(),
        hiddenTopics = emptySet(),
        following = emptySet(),
        gone = emptySet(),
    )

    private fun id(n: Int) = n.toString().padStart(32, '0')

    @Test
    fun `items whose listing does not decode or names another channel are dropped`() {
        val items = listOf(
            DirectoryItemWire(id(1), "a", "10+"),
            DirectoryItemWire(id(2), "bad", "10+"),
            DirectoryItemWire(id(3), "wrong", "weird"),
            DirectoryItemWire(id(1), "a", "10+"),
        )
        val cards = cardsFromItems(items, CardSource.BUCKET) { b ->
            when (b) {
                "a" -> listing(id(1))
                "wrong" -> listing(id(9))
                else -> null
            }
        }
        assertEquals(listOf(id(1)), cards.map { it.channelIdHex })
        assertEquals("10+", cards.single().band)
    }

    @Test
    fun `hidden, not-interested, followed and gone cards are filtered`() {
        val c = cardsFromItems(listOf(DirectoryItemWire(id(1), "a", "10+")), CardSource.BUCKET) { listing(id(1)) }.single()
        assertTrue(cardVisible(c, ctx))
        assertFalse(cardVisible(c, ctx.copy(notInterested = setOf(id(1)))))
        assertFalse(cardVisible(c, ctx.copy(following = setOf(id(1)))))
        assertFalse(cardVisible(c, ctx.copy(gone = setOf(id(1)))))
        assertFalse(cardVisible(c, ctx.copy(hiddenTopics = setOf(1))))
    }

    @Test
    fun `the score is the web weights and the order is total`() {
        val base = DirectoryCard(id(1), "h", "B", "d", 1, "en", "1k+", CardSource.BUCKET, 0, null)
        assertEquals(100 + 50 + 30 + 100, scoreCard(base, ctx))
        val grouped = base.copy(topic = 7, language = "de", band = null, serverRank = 1000,
            followers = GroupFollowers("g", "G", 9, emptyList()))
        assertEquals(200, scoreCard(grouped, ctx))
        val tie1 = base.copy(channelIdHex = id(2), name = "A")
        val tie2 = base.copy(channelIdHex = id(3), name = "A")
        assertEquals(listOf(id(2), id(3), id(1)), rankCards(listOf(base, tie2, tie1), ctx).map { it.channelIdHex })
    }

    @Test
    fun `a channel seen twice merges into one card with the listing and the followers`() {
        val bucket = DirectoryCard(id(1), "h", "N", "desc", 1, "en", "10+", CardSource.BUCKET, 4, null)
        val group = DirectoryCard(id(1), "h", "N", null, null, null, null, CardSource.GROUP, 1000,
            GroupFollowers("g", "Book club", 2, emptyList()))
        val merged = rankCards(listOf(group, bucket), ctx).single()
        assertEquals("desc", merged.description)
        assertEquals(4, merged.serverRank)
        assertEquals(CardSource.GROUP, merged.source)
        assertEquals("Book club", merged.followers?.groupTitle)
    }

    @Test
    fun `the reason line counts people and names only contacts`() {
        val c = DirectoryCard(id(1), "h", "N", null, 3, null, null, CardSource.GROUP, 0,
            GroupFollowers("g", "Book club", 3, listOf("Anna")))
        val group = mapOf("group" to "Book club")
        assertEquals(
            UiText("discover.reason.followed_by_others", mapOf("name" to "Anna") + group, counts = mapOf("count" to 2uL)),
            reasonLine(c),
        )
        assertEquals(
            UiText("discover.reason.followed_by", mapOf("name" to "Anna") + group),
            reasonLine(c.copy(followers = c.followers!!.copy(count = 1))),
        )
        assertEquals(
            UiText("discover.reason.followed_by_others", mapOf("name" to "Anna") + group, counts = mapOf("count" to 1uL)),
            reasonLine(c.copy(followers = c.followers!!.copy(count = 2))),
        )
        assertEquals(
            UiText("discover.reason.followed_count", group, counts = mapOf("count" to 1uL)),
            reasonLine(c.copy(followers = GroupFollowers("g", "Book club", 1, emptyList()))),
        )
        assertEquals(UiText("discover.reason.search"), reasonLine(c.copy(followers = null, source = CardSource.SEARCH)))
        assertEquals(
            UiText("discover.reason.popular", parts = mapOf("topic" to UiText("discover.topic.business"))),
            reasonLine(c.copy(followers = null, source = CardSource.BUCKET, topic = 4)),
        )
        assertEquals(
            UiText("discover.reason.popular", parts = mapOf("topic" to UiText("discover.topic.other"))),
            reasonLine(c.copy(followers = null, topic = null)),
        )
    }

    @Test
    fun `subscribing needs the handle to resolve to the card's channel`() {
        assertEquals(VerifyDecision.OK, verifyDecision("AB", "ab"))
        assertEquals(VerifyDecision.GONE, verifyDecision("ab", null))
        assertEquals(VerifyDecision.GONE, verifyDecision("ab", "cd"))
    }

    @Test
    fun `suggestions are mixed after every N items, never first or last`() {
        assertEquals(emptyMap<Int, Int>(), cardsAfterItems(40, 5, SuggestionRate.NEVER))
        assertEquals(mapOf(7 to 0, 15 to 1), cardsAfterItems(20, 5, SuggestionRate.SOMETIMES))
        assertEquals(mapOf(7 to 0), cardsAfterItems(16, 1, SuggestionRate.SOMETIMES))
        assertEquals(emptyMap<Int, Int>(), cardsAfterItems(8, 3, SuggestionRate.SOMETIMES))
        assertEquals(mapOf(19 to 0), cardsAfterItems(21, 3, SuggestionRate.RARELY))
    }

    // --- prefs codec -------------------------------------------------------

    @Test
    fun `prefs round-trip and a malformed field falls back alone`() {
        val p = DiscoverPrefs(
            topics = listOf(1, 2),
            language = "de",
            mixRandom = false,
            notInterested = listOf(id(1)),
            hiddenTopics = listOf(5),
            decoys = DecoyDraw("2026-09", "de", allTopics),
            lastRefreshMs = 5,
            lastFetchedKey = "de:1,2",
            buckets = mapOf("1:de" to CachedBucket(5, listOf(DirectoryItemWire(id(1), "x", "10+")))),
            introDismissed = true,
        )
        assertEquals(p, decodeDiscoverPrefs(encodeDiscoverPrefs(p)))
        assertEquals(DiscoverPrefs(), decodeDiscoverPrefs(null))
        assertEquals(DiscoverPrefs(), decodeDiscoverPrefs("not json"))
        assertEquals(DiscoverPrefs(), decodeDiscoverPrefs("""{"version":2,"topics":[1]}"""))
        val lenient = decodeDiscoverPrefs(
            """{"version":1,"topics":[1,999,1],"language":"English","notInterested":["zz","${id(2)}"],""" +
                """"buckets":{"bad":{"fetchedAtMs":1},"2:en":{"fetchedAtMs":1,"items":[{"channel_id":"nope"}]}}}""",
        )
        assertEquals(listOf(1), lenient.topics)
        assertNull(lenient.language)
        assertEquals(listOf(id(2)), lenient.notInterested)
        assertEquals(setOf("2:en"), lenient.buckets.keys)
        assertEquals(emptyList<DirectoryItemWire>(), lenient.buckets["2:en"]!!.items)
    }

    @Test
    fun `the app language and the offered list`() {
        assertEquals("pt", appLanguage("pt-BR"))
        assertEquals("en", appLanguage(""))
        assertEquals("en", appLanguage("x"))
        assertEquals("sv", languageOptions("sv").first())
        assertEquals(LANGUAGE_CODES, languageOptions("en", null))
    }

    @Test
    fun `a listing stamp is strictly after the stored one`() {
        assertEquals(100uL, nextListingStamp(100, null))
        assertEquals(100uL, nextListingStamp(100, 99uL))
        assertEquals(101uL, nextListingStamp(100, 100uL))
        assertNotEquals(0uL, nextListingStamp(-5, 0uL))
    }

    // --- held channels in search results (UX spec section 6.2) -------------

    private val idA = "aa".repeat(16)
    private val idB = "bb".repeat(16)
    private val idC = "cc".repeat(16)

    @Test
    fun `a channel this device publishes is its own, before following`() {
        assertEquals(CardRelation.OWN, cardRelation(idA, setOf(idA), setOf(idA)))
        assertEquals(UiText("channels.row.own"), relationLine(CardRelation.OWN))
    }

    @Test
    fun `a channel only in the list is followed`() {
        assertEquals(CardRelation.FOLLOWING, cardRelation(idB, setOf(idA), setOf(idA, idB)))
        assertEquals(UiText("discover.card.following"), relationLine(CardRelation.FOLLOWING))
    }

    @Test
    fun `any other channel is left to Subscribe, whatever the case of its id`() {
        assertEquals(CardRelation.NONE, cardRelation(idC, setOf(idA), setOf(idB)))
        assertNull(relationLine(CardRelation.NONE))
        assertEquals(CardRelation.OWN, cardRelation(idA.uppercase(), setOf(idA), emptySet()))
        assertEquals(CardRelation.FOLLOWING, cardRelation(idB, emptySet(), setOf(idB.uppercase())))
    }
}
