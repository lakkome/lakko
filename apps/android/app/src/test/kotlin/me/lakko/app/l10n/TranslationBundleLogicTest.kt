// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.l10n

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder

/**
 * Tests for the ADR-0061 S6 decisions in [TranslationBundleLogic]. The core
 * acceptance call is replaced by a fake [BundleVerifier] (a host JVM has no
 * native library); the rules the core enforces are its own tests'.
 */
class TranslationBundleLogicTest {

    @get:Rule
    val tmp = TemporaryFolder()

    /** A fake core: accepts bytes it knows, at their version, above the floor. */
    private class FakeCore(private val known: Map<String, Pair<ULong, Long>>) : BundleVerifier<String> {
        val calls = mutableListOf<Pair<String, ULong?>>()

        override fun accept(bytes: ByteArray, lastAccepted: ULong?): AcceptedBundle<String>? {
            val id = bytes.decodeToString()
            calls += id to lastAccepted
            val (version, signedAt) = known[id] ?: return null
            if (lastAccepted != null && version <= lastAccepted) return null
            return AcceptedBundle(id, version, signedAt)
        }
    }

    private fun stored(id: String, version: ULong, signedAt: Long = 7L) =
        StoredBundle(id.encodeToByteArray(), StoredBundleMeta(version, signedAt))

    // ---- launch ----

    @Test
    fun nothing_stored_is_none_and_never_calls_the_core() {
        val core = FakeCore(emptyMap())
        assertEquals(LaunchOutcome.None, decideOnLaunch(null, core))
        assertTrue(core.calls.isEmpty())
    }

    @Test
    fun a_stored_bundle_that_still_verifies_is_used_with_no_rollback_floor() {
        val core = FakeCore(mapOf("b3" to (3uL to 1234L)))
        val outcome = decideOnLaunch(stored("b3", 3uL), core)
        assertEquals(LaunchOutcome.Use(AcceptedBundle("b3", 3uL, 1234L)), outcome)
        assertEquals(listOf("b3" to null), core.calls)
    }

    @Test
    fun a_stored_bundle_the_core_now_refuses_is_discarded() {
        // e.g. an app update raised the built-in version past it.
        val core = FakeCore(emptyMap())
        assertEquals(LaunchOutcome.Discard, decideOnLaunch(stored("b3", 3uL), core))
    }

    @Test
    fun a_stored_bundle_whose_version_disagrees_with_its_meta_is_discarded() {
        val core = FakeCore(mapOf("b3" to (3uL to 1L)))
        assertEquals(LaunchOutcome.Discard, decideOnLaunch(stored("b3", 2uL), core))
    }

    // ---- download ----

    @Test
    fun a_newer_download_is_applied_against_the_stored_floor() {
        val core = FakeCore(mapOf("b4" to (4uL to 99L)))
        val outcome = decideOnDownload("b4".encodeToByteArray(), 3uL, 1024, core)
        assertEquals(DownloadOutcome.Apply(AcceptedBundle("b4", 4uL, 99L)), outcome)
        assertEquals(listOf("b4" to 3uL), core.calls)
    }

    @Test
    fun the_same_or_an_older_download_is_kept_out_rollback() {
        val core = FakeCore(mapOf("b3" to (3uL to 1L), "b2" to (2uL to 1L)))
        assertEquals(DownloadOutcome.Keep, decideOnDownload("b3".encodeToByteArray(), 3uL, 1024, core))
        assertEquals(DownloadOutcome.Keep, decideOnDownload("b2".encodeToByteArray(), 3uL, 1024, core))
    }

    @Test
    fun a_refused_download_is_kept_out() {
        val core = FakeCore(emptyMap())
        assertEquals(DownloadOutcome.Keep, decideOnDownload("forged".encodeToByteArray(), null, 1024, core))
    }

    @Test
    fun empty_or_oversized_bodies_never_reach_the_core() {
        val core = FakeCore(mapOf("" to (9uL to 1L)))
        assertEquals(DownloadOutcome.Keep, decideOnDownload(ByteArray(0), null, 1024, core))
        assertEquals(DownloadOutcome.Keep, decideOnDownload(ByteArray(1025), null, 1024, core))
        assertTrue(core.calls.isEmpty())
    }

    // ---- cadence ----

    @Test
    fun never_checked_is_due() {
        assertTrue(isTranslationFetchDue(null, 1_000L, DAY))
    }

    @Test
    fun within_the_window_is_not_due_and_after_it_is() {
        val last = 1_000_000_000_000L
        assertFalse(isTranslationFetchDue(last, last + DAY - 1, DAY))
        assertTrue(isTranslationFetchDue(last, last + DAY, DAY))
    }

    @Test
    fun a_clock_that_went_backwards_is_due() {
        assertTrue(isTranslationFetchDue(5_000L, 1_000L, DAY))
    }

    // ---- persistence format ----

    @Test
    fun meta_round_trips_including_the_largest_version() {
        val meta = StoredBundleMeta(ULong.MAX_VALUE, -5L)
        assertEquals(meta, StoredBundleMetaCodec.decode(StoredBundleMetaCodec.encode(meta)))
        assertEquals("v1\n3\n42\n", StoredBundleMetaCodec.encode(StoredBundleMeta(3uL, 42L)))
    }

    @Test
    fun malformed_meta_reads_as_absent() {
        for (bad in listOf("", "v2\n3\n4\n", "v1\n-3\n4\n", "v1\n3\n", "v1\nx\n4\n", "v1\n3\n4\n5\n")) {
            assertNull(bad, StoredBundleMetaCodec.decode(bad))
        }
    }

    @Test
    fun file_storage_writes_reads_replaces_and_deletes() {
        val store = FileTranslationBundleStorage(tmp.newFolder("l10n"))
        assertNull(store.read())
        store.write(stored("first", 2uL, 10L))
        store.write(stored("second", 3uL, 20L))
        val back = store.read()!!
        assertArrayEquals("second".encodeToByteArray(), back.bytes)
        assertEquals(StoredBundleMeta(3uL, 20L), back.meta)
        store.delete()
        assertNull(store.read())
    }

    @Test
    fun file_storage_with_bytes_but_no_meta_reads_as_absent() {
        val dir = tmp.newFolder("l10n")
        java.io.File(dir, "translation-bundle.bin").writeBytes(byteArrayOf(1, 2, 3))
        assertNull(FileTranslationBundleStorage(dir).read())
    }

    @Test
    fun file_storage_creates_its_directory() {
        val dir = java.io.File(tmp.root, "not-yet")
        val store = FileTranslationBundleStorage(dir)
        store.write(stored("b", 1uL))
        assertEquals(1uL, store.read()!!.meta.version)
    }

    private companion object {
        const val DAY = 24L * 60 * 60 * 1000
    }
}
