// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * ADR-0087 section 15 — only the owner rotates; a publisher seals under an
 * epoch the channel service has reached, reads with the composer's seed only
 * for the epoch it serves, keeps the outgoing seed when the owner's commit
 * moves it, and owes the channel record until it lands. The twin of web
 * `publisher-seed.test.ts`.
 */
class PublisherSeedsTest {

    private val signing = ByteArray(32) { 9 }
    private fun seed(b: Int) = ByteArray(32) { b.toByte() }
    private fun keys(seedByte: Int, seedEpoch: UInt?) = ChannelPublisherKeys(seed(seedByte), signing, seedEpoch)

    // --- sealing ---------------------------------------------------------------

    @Test
    fun the_service_at_the_composer_epoch_seals_with_the_composer_seed() {
        val s = composerSeedFor(keys(5, 5u), 5u) { null }!!
        assertEquals(5u, s.epoch)
        assertArrayEquals(seed(5), s.seed)
    }

    @Test
    fun a_service_ahead_seals_under_the_composer_epoch_never_above_it() {
        // This device has not applied the owner's later commit yet.
        val s = composerSeedFor(keys(5, 5u), 7u) { null }!!
        assertEquals(5u, s.epoch)
    }

    @Test
    fun a_service_behind_seals_with_the_held_seed_of_its_epoch() {
        // A co-publisher applied the owner's commit (5) before the reseal
        // moved the service from 4.
        val held = mapOf(4u to seed(4))
        val s = composerSeedFor(keys(5, 5u), 4u) { held[it] }!!
        assertEquals(4u, s.epoch)
        assertArrayEquals(seed(4), s.seed)
    }

    @Test
    fun a_service_behind_with_no_held_seed_refuses_the_post() {
        // A new publisher joined before the owner's reseal landed.
        assertNull(composerSeedFor(keys(5, 5u), 4u) { null })
    }

    @Test
    fun keys_with_no_seed_epoch_seal_under_the_current_epoch() {
        assertEquals(3u, composerSeedFor(keys(1, null), 3u) { null }!!.epoch)
    }

    // --- reading ---------------------------------------------------------------

    @Test
    fun the_composer_seed_opens_only_the_epoch_it_serves() {
        val k = keys(5, 5u)
        val held = mapOf(4u to seed(4))
        assertArrayEquals(seed(5), publisherReadSeed(k, 4u, 5u) { held[it] })
        assertArrayEquals(seed(4), publisherReadSeed(k, 4u, 4u) { held[it] })
        assertNull(publisherReadSeed(k, 4u, 3u) { held[it] })
    }

    @Test
    fun a_reader_reads_from_its_held_seeds() {
        assertArrayEquals(seed(2), publisherReadSeed(null, 2u, 2u) { if (it == 2u) seed(2) else null })
    }

    // --- the owner's commit applied on a co-publisher ---------------------------

    @Test
    fun a_moved_seed_keeps_the_old_one_under_its_epoch_and_files_the_new_one() {
        val move = movePublisherSeed(keys(4, 4u), 5u, seed(5), emptyMap())
        assertEquals(4u, move.kept!!.first)
        assertArrayEquals(seed(4), move.kept!!.second)
        assertEquals(5u, move.keys!!.seedEpoch)
        assertArrayEquals(seed(5), move.keys!!.seed)
        assertArrayEquals("the signing seed is kept", signing, move.keys!!.signingKeySeed)
        assertEquals(5u, move.filed!!.first)
    }

    @Test
    fun the_same_seed_at_the_same_epoch_changes_nothing() {
        val move = movePublisherSeed(keys(5, 5u), 5u, seed(5), mapOf(5u to seed(5)))
        assertNull(move.kept)
        assertNull(move.keys)
        assertNull(move.filed)
    }

    @Test
    fun a_seed_already_filed_for_an_epoch_is_not_replaced() {
        val move = movePublisherSeed(keys(4, 4u), 5u, seed(5), mapOf(4u to seed(40), 5u to seed(50)))
        assertNull(move.kept)
        assertNull(move.filed)
    }

    @Test
    fun keys_with_no_seed_epoch_keep_the_old_seed_under_the_epoch_before() {
        val move = movePublisherSeed(keys(4, null), 5u, seed(5), emptyMap())
        assertEquals(4u, move.kept!!.first)
    }

    // --- rotation -----------------------------------------------------------------

    @Test
    fun only_the_owner_rotates() {
        assertTrue(rotatesChannelKeyHere(isOwner = true))
        assertFalse(rotatesChannelKeyHere(isOwner = false))
    }

    // --- the owed channel record ---------------------------------------------------

    private class Marks : OwedRecordMarks {
        val owed = HashSet<String>()
        override fun isOwed(channelId: String) = channelId in owed
        override fun mark(channelId: String) { owed += channelId }
        override fun clear(channelId: String) { owed -= channelId }
    }

    @Test
    fun a_failed_record_send_stays_owed_and_a_later_landing_clears_it() = runBlocking {
        val marks = Marks()
        var sawOwedDuringSend = false
        assertFalse(reissueRecordOwed(marks, "c") { sawOwedDuringSend = marks.isOwed("c"); false })
        assertTrue("owed before the attempt", sawOwedDuringSend)
        assertTrue(marks.isOwed("c"))
        assertTrue(reissueRecordOwed(marks, "c") { true })
        assertFalse(marks.isOwed("c"))
    }

}
