// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.storage

import kotlinx.coroutines.test.runTest
import me.lakko.app.profile.AvatarRefs
import me.lakko.app.profile.avatarBlobOf
import me.lakko.app.profile.replacedAvatarBlobs
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** ADR-0065 sections 3 and 6 — the durable delete list and the keep
 *  schedule. */
class BlobLifetimeTest {
    private fun hex(n: Int) = "%02x".format(n).repeat(32)

    @Test
    fun owed_deletes_are_kept_per_account_and_only_failures_stay() = runTest {
        val storage = InMemoryBlobLifetimeStorage()
        val queue = BlobDeleteQueue(storage)
        queue.add("a", listOf(hex(1), hex(2), hex(3), hex(1), "not-hex"))
        queue.add("b", listOf(hex(9)))
        assertEquals(listOf(hex(1), hex(2), hex(3)), queue.owed("a"))
        val remaining = queue.drain("a") { id ->
            when (id) {
                hex(1) -> true
                hex(2) -> false // refused for good: dropped
                else -> throw RuntimeException("network")
            }
        }
        assertEquals(1, remaining)
        assertEquals(listOf(hex(3)), queue.owed("a"))
        assertEquals(listOf(hex(9)), queue.owed("b"))
        queue.clear("a")
        assertTrue(queue.owed("a").isEmpty())
    }

    @Test
    fun the_list_is_bounded() {
        val queue = BlobDeleteQueue(InMemoryBlobLifetimeStorage())
        queue.add("a", (0 until BlobDeleteQueue.MAX_OWED + 5).map { "%064x".format(it) })
        assertEquals(BlobDeleteQueue.MAX_OWED, queue.owed("a").size)
    }

    @Test
    fun a_new_bucket_is_not_kept_at_once_and_comes_due_about_monthly() {
        var now = 1_000_000_000_000L
        val schedule = BlobKeepSchedule(InMemoryBlobLifetimeStorage(), now = { now })
        val buckets = (1..6).map { KeepBucket("pins:$it", listOf(hex(it))) }
        assertTrue(schedule.due("a", buckets).isEmpty())
        // Each falls due within its first 30 days, at most four per tick.
        now += BlobKeepSchedule.KEEP_SPREAD_MS
        val first = schedule.due("a", buckets)
        assertEquals(BlobKeepSchedule.MAX_PER_TICK, first.size)
        first.forEach { schedule.markKept("a", it.key) }
        val second = schedule.due("a", buckets)
        assertEquals(2, second.size)
        assertTrue(second.none { b -> first.any { it.key == b.key } })
        second.forEach { schedule.markKept("a", it.key) }
        assertTrue(schedule.due("a", buckets).isEmpty())
        now += BlobKeepSchedule.KEEP_INTERVAL_MS
        assertEquals(BlobKeepSchedule.MAX_PER_TICK, schedule.due("a", buckets).size)
    }

    @Test
    fun the_first_due_offset_is_the_shared_fnv1a_rule() {
        // The same value web and iOS pin for "saved".
        assertEquals(1_203_033_924L, BlobKeepSchedule.offsetOf("saved"))
        var now = 0L
        val schedule = BlobKeepSchedule(InMemoryBlobLifetimeStorage(), now = { now })
        val saved = listOf(KeepBucket("saved", listOf(hex(1))))
        schedule.due("a", saved)
        now = 1_203_033_923
        assertTrue(schedule.due("a", saved).isEmpty())
        now = 1_203_033_924
        assertEquals(1, schedule.due("a", saved).size)
    }

    @Test
    fun first_due_points_are_spread_over_thirty_days() {
        val offsets = (1..200).map { BlobKeepSchedule.offsetOf("pins:$it") }
        assertTrue(offsets.all { it in 0 until BlobKeepSchedule.KEEP_INTERVAL_MS })
        assertTrue(offsets.toSet().size > 150)
        assertEquals(BlobKeepSchedule.KEEP_INTERVAL_MS, BlobKeepSchedule.KEEP_SPREAD_MS)
    }

    @Test
    fun the_delete_key_seed_is_the_lineage_seed_across_a_rotation() {
        val original = ByteArray(32) { 1 }
        val rotated = ByteArray(32) { 2 }
        // Before any rotation the current seed is the lineage.
        val before = blobDeleteSeedOf(predecessorSeed = null, currentSeed = original)
        // After a rotation the current seed changed, the earliest predecessor
        // is the original one: the delete keys stay the same.
        val after = blobDeleteSeedOf(predecessorSeed = original, currentSeed = rotated)
        assertTrue(before!!.contentEquals(after!!))
    }

    @Test
    fun an_empty_bucket_is_never_due_and_gone_buckets_are_forgotten() {
        var now = 0L
        val storage = InMemoryBlobLifetimeStorage()
        val schedule = BlobKeepSchedule(storage, now = { now })
        schedule.due("a", listOf(KeepBucket("pins:x", emptyList())))
        now += BlobKeepSchedule.KEEP_INTERVAL_MS * 2
        assertTrue(schedule.due("a", listOf(KeepBucket("pins:x", emptyList()))).isEmpty())
        schedule.due("a", emptyList())
        assertTrue(storage.isEmptyFor("a"))
    }

    @Test
    fun keeps_go_out_in_batches_of_at_most_256() {
        val ids = (0 until 600).map { "%064x".format(it) }
        assertEquals(listOf(256, 256, 88), keepBatches(ids).map { it.size })
    }

    @Test
    fun a_replaced_photo_deletes_only_what_the_new_one_does_not_name() {
        fun blob(n: Int) = avatarBlobOf(ByteArray(32) { n.toByte() }, ByteArray(32), ByteArray(12), 10, 10)
        val old = AvatarRefs(blob(1), blob(2), 5000)
        assertEquals(2, replacedAvatarBlobs(old, null).size)
        assertEquals(listOf(1.toByte()), replacedAvatarBlobs(old, AvatarRefs(blob(3), blob(2), 5000)).map { it[0] })
        assertTrue(replacedAvatarBlobs(null, old).isEmpty())
    }
}
