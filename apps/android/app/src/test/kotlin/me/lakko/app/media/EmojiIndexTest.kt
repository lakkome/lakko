// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.media

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * ADR-0047 §3 / UX spec §4 — emoji search, the Recent row, skin-tone defaults
 * and the one-grapheme reaction check, against the REAL bundled dataset: the
 * web client's `emoji-data.json`, whose path Gradle hands the test JVM, so the
 * Android index is proven against the same file the app ships.
 */
class EmojiIndexTest {

    private val dataset: EmojiDataset by lazy {
        val path = System.getProperty("lakko.emojiDataPath")
            ?: error("lakko.emojiDataPath not set — run through Gradle")
        parseEmojiDataset(File(path).readText())
    }

    @Test
    fun datasetLoadsEveryGroupAndResolvesTonesToTheirBase() {
        assertEquals("Smileys & Emotion", dataset.groups.first())
        assertFalse("Component" in dataset.groups)
        assertTrue(dataset.entries.size > 1500)
        assertEquals("waving hand", dataset.byEmoji["👋🏽"]?.name)
    }

    @Test
    fun malformedRowsAreDroppedNotFatal() {
        val ds = parseEmojiDataset(
            """{"groups":["A"],"emoji":[["x","ok","",0],["y",5,"",0],["z","bad group","",9],["w","short"]]}""",
        )
        assertEquals(listOf("ok"), ds.entries.map { it.name })
        assertEquals(0, parseEmojiDataset("{not json").entries.size)
        assertEquals(0, parseEmojiDataset("""{"groups":"x","emoji":[]}""").entries.size)
    }

    @Test
    fun searchRanksNamesFirstAndRequiresEveryToken() {
        val cats = searchEmoji(dataset, "cat")
        assertEquals("cat", cats.first().name)
        assertTrue(cats.any { it.name == "cat face" })
        val hearts = searchEmoji(dataset, "red heart")
        assertEquals("red heart", hearts.first().name)
        assertTrue(hearts.all { (it.name + it.keywords.joinToString(" ")).contains("red") })
    }

    @Test
    fun searchFindsByKeywordAloneAndNothingForNonsense() {
        assertTrue(searchEmoji(dataset, "grin").any { it.emoji == "😀" })
        assertTrue(searchEmoji(dataset, "   ").isEmpty())
        assertTrue(searchEmoji(dataset, "zzqxv").isEmpty())
    }

    @Test
    fun recentMovesAReuseToTheFrontAndCapsAt24() {
        var list = emptyList<String>()
        for (i in 0 until 30) list = pushRecent(list, dataset.entries[i].emoji)
        assertEquals(MAX_RECENT_EMOJI, list.size)
        val again = pushRecent(list, list[10])
        assertEquals(list[10], again.first())
        assertEquals(MAX_RECENT_EMOJI, again.toSet().size)
    }

    @Test
    fun recentRoundTripsAndDropsAnythingThatIsNotAnEmoji() {
        val stored = encodeRecent(listOf("😀", "👍"))
        assertEquals(listOf("😀", "👍"), decodeRecent(stored))
        assertEquals(listOf("😀"), decodeRecent("""["ok?","😀",5]"""))
        assertEquals(emptyList<String>(), decodeRecent("{not json"))
        assertEquals(emptyList<String>(), decodeRecent(null))
    }

    @Test
    fun aChosenToneBecomesTheDefaultAndChoosingTheBaseClearsIt() {
        val wave = dataset.byEmoji.getValue("👋")
        val dark = wave.tones!![4]
        var prefs = withTonePref(emptyMap(), wave, dark)
        assertEquals(dark, displayEmoji(wave, prefs))
        assertEquals(dark, displayEmoji(wave, decodeTonePrefs(encodeTonePrefs(prefs))))
        prefs = withTonePref(prefs, wave, wave.emoji)
        assertEquals(wave.emoji, displayEmoji(wave, prefs))
        // A stored variant that is not one of the entry's tones is ignored.
        assertEquals(wave.emoji, displayEmoji(wave, mapOf(wave.emoji to "😀")))
    }

    @Test
    fun oneEmojiGraphemeAccepted() {
        for (ok in listOf(
            "👍", // thumbs up
            "❤️", // red heart with VS16
            "👍🏽", // thumbs up, medium skin tone
            "👩‍💻", // woman technologist (ZWJ)
            "🇫🇮", // flag: Finland
            "#️⃣", // keycap #
            "1⃣", // keycap 1 without VS16
        )) {
            assertTrue("expected one emoji grapheme: $ok", isSingleEmojiGrapheme(ok))
        }
    }

    @Test
    fun anythingElseRefused() {
        for (bad in listOf(
            "",
            "a",
            "1",
            "👍👍", // two emoji
            "👍x", // emoji + text
            "ok",
            "🇫", // a lone regional indicator
            "👍".repeat(20), // over the byte bound
        )) {
            assertFalse("expected refusal: $bad", isSingleEmojiGrapheme(bad))
        }
    }

    @Test
    fun everyEmojiThePickerCanProducePassesTheReactionCheck() {
        // The picker only offers dataset entries and their tones, so the
        // reaction check must accept every one of them or a pick is silently
        // dropped.
        val refused = dataset.entries
            .flatMap { listOf(it.emoji) + it.tones.orEmpty() }
            .filterNot(::isSingleEmojiGrapheme)
        assertEquals(emptyList<String>(), refused)
    }
}
