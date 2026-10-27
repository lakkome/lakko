// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.avatar

import me.lakko.app.profile.AvatarBlob
import me.lakko.app.profile.AvatarRefs
import me.lakko.app.profile.avatarBlobOf
import me.lakko.app.profile.avatarRefsOf
import me.lakko.app.profile.toFfi
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File
import java.nio.file.Files

/** ADR-0084 §3 — the decrypted-avatar cache and the reference conversions. */
class AvatarCacheTest {
    private val id = "ab".repeat(32)

    /** A seal that flips every byte, so the file on disk is never the
     *  plaintext. */
    private object FlipSeal : AvatarSeal {
        override fun seal(plaintext: ByteArray) = ByteArray(plaintext.size) { (plaintext[it].toInt() xor 0xff).toByte() }
        override fun open(sealed: ByteArray) = seal(sealed)
    }

    private fun dir(): File = Files.createTempDirectory("avatars").toFile()

    @Test
    fun put_then_get_from_disk_after_a_restart_and_the_file_is_sealed() {
        val d = dir()
        val bytes = byteArrayOf(1, 2, 3)
        assertTrue(AvatarCache(d, FlipSeal).put(id, bytes))
        val onDisk = File(d, "$id.bin").readBytes()
        assertFalse(onDisk.contentEquals(bytes))
        // A fresh cache (a new process) reads it back from disk.
        assertArrayEquals(bytes, AvatarCache(d, FlipSeal).get(id))
    }

    @Test
    fun an_unreadable_entry_is_dropped() {
        val d = dir()
        AvatarCache(d, PlainAvatarSeal).put(id, byteArrayOf(9))
        val refusing = object : AvatarSeal {
            override fun seal(plaintext: ByteArray): ByteArray? = null
            override fun open(sealed: ByteArray): ByteArray? = null
        }
        assertNull(AvatarCache(d, refusing).get(id))
        assertFalse(File(d, "$id.bin").exists())
    }

    @Test
    fun a_failure_waits_one_minute() {
        var now = 1_000L
        val cache = AvatarCache(dir(), PlainAvatarSeal, now = { now })
        assertTrue(cache.mayTry(id))
        cache.markFailed(id)
        assertFalse(cache.mayTry(id))
        now += AvatarCache.RETRY_AFTER_MS - 1
        assertFalse(cache.mayTry(id))
        now += 1
        assertTrue(cache.mayTry(id))
    }

    @Test
    fun a_key_that_is_not_a_blob_id_never_becomes_a_path() {
        val cache = AvatarCache(dir(), PlainAvatarSeal)
        assertFalse(cache.put("../../etc", byteArrayOf(1)))
        assertNull(cache.get("../../etc"))
        assertFalse(cache.put("AB".repeat(32), byteArrayOf(1)))
    }

    @Test
    fun clear_forgets_everything() {
        val d = dir()
        val cache = AvatarCache(d, PlainAvatarSeal)
        cache.put(id, byteArrayOf(1))
        cache.markFailed("cd".repeat(32))
        cache.clear()
        assertNull(cache.get(id))
        assertTrue(cache.mayTry("cd".repeat(32)))
        assertFalse(d.exists())
    }

    @Test
    fun refs_round_trip_through_the_wire_form() {
        val still = avatarBlobOf(ByteArray(32) { 1 }, ByteArray(32) { 2 }, ByteArray(12) { 3 }, 1000, 640)
        val video = avatarBlobOf(ByteArray(32) { 4 }, ByteArray(32) { 5 }, ByteArray(12) { 6 }, 200_000, 480)
        val refs = AvatarRefs(still, video, 9_500)
        val wire = refs.toFfi()
        assertNotNull(wire)
        assertEquals(480.toUShort(), wire!!.video!!.side)
        assertEquals(9_500.toUShort(), wire.videoDurationMs)
        assertEquals(refs, avatarRefsOf(wire))
        // A photo alone.
        assertEquals(AvatarRefs(still), avatarRefsOf(AvatarRefs(still).toFfi()))
    }

    @Test
    fun a_malformed_stored_ref_is_not_sent() {
        assertNull(AvatarRefs(AvatarBlob("zz", "", "", 1, 1)).toFfi())
        // A video with an out-of-bound duration goes out as the still alone.
        val still = avatarBlobOf(ByteArray(32), ByteArray(32), ByteArray(12), 10, 10)
        val wire = AvatarRefs(still, still, 20_000).toFfi()
        assertNull(wire!!.video)
    }

    @Test
    fun sealed_refs_round_trip_and_fail_closed() {
        val refs = AvatarRefs(avatarBlobOf(ByteArray(32), ByteArray(32), ByteArray(12), 10, 10))
        val stored = sealAvatarRefs(FlipSeal, refs)
        assertEquals(refs, openAvatarRefs(FlipSeal, stored))
        assertNull(openAvatarRefs(FlipSeal, "not base64 !"))
        assertNull(sealAvatarRefs(FlipSeal, null))
    }
}
