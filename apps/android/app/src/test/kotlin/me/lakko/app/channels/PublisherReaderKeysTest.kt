// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

/**
 * UX 15.1, live walk 2026-11-24 ("Night walk": web owner, iOS publisher):
 * each publishing account's reader key, announced on the PublisherGroup, is
 * left out of the other publishers' subscriber list and count. The envelope
 * codec is an FFI call the host JVM cannot load; this covers the rules
 * around it (they run in Kotlin).
 */
class PublisherReaderKeysTest {
    private val channel = ByteArray(16) { 0x0C }
    private val owner = ByteArray(32) { 0x0A }
    private val publisher = ByteArray(32) { 0x0B }
    private val stranger = ByteArray(32) { 0x0E }
    private val reader = ByteArray(32) { 0xD7.toByte() }

    private fun b64(b: ByteArray) = java.util.Base64.getEncoder().encodeToString(b)

    @Test
    fun keptOnlyForThisChannelFromACurrentPublisher() {
        val accounts = listOf(owner, publisher)
        assertEquals(b64(publisher) to "d7".repeat(32), PublisherReaderKeys.verdict(channel, channel, reader, publisher, accounts))
        assertNull(PublisherReaderKeys.verdict(channel, channel, reader, stranger, accounts))
        assertNull(PublisherReaderKeys.verdict(channel, channel, reader, null, accounts))
        assertNull(PublisherReaderKeys.verdict(channel, ByteArray(16) { 1 }, reader, publisher, accounts))
        assertNull(PublisherReaderKeys.verdict(channel, channel, ByteArray(31), publisher, accounts))
    }

    @Test
    fun leftOutOnlyWhileTheAccountPublishes() {
        val stored = mapOf(b64(owner) to "aa".repeat(32), b64(publisher) to "bb".repeat(32))
        assertEquals(mapOf(b64(owner) to "aa".repeat(32)), PublisherReaderKeys.pruned(stored, listOf(owner)))
        assertEquals(stored, PublisherReaderKeys.pruned(stored, listOf(owner, publisher)))
    }

    @Test
    fun sentOnceRegisteredAndAgainWhenTheGroupGrows() {
        assertNull(PublisherReaderKeys.sendDue(null, 2, null))
        val first = PublisherReaderKeys.sendDue("AB", 2, null)
        assertNotNull(first)
        assertNull(PublisherReaderKeys.sendDue("ab", 2, first))
        assertNotNull(PublisherReaderKeys.sendDue("ab", 3, first))
    }

    @Test
    fun theStoredMapRoundTripsAndDropsMalformedEntries() {
        val map = mapOf(b64(owner) to "aa".repeat(32), b64(publisher) to "bb".repeat(32))
        assertEquals(map, PublisherReaderKeys.decode(PublisherReaderKeys.encode(map)))
        assertEquals(
            mapOf(b64(owner) to "aa".repeat(32)),
            PublisherReaderKeys.decode("${b64(owner)}=${"AA".repeat(32)};short=aa;${b64(publisher)}=zz"),
        )
        assertEquals(emptyMap<String, String>(), PublisherReaderKeys.decode(null))
    }
}
