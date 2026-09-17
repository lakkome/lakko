// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Test
import uniffi.lakko_ffi_uniffi.FfiChannelMeta
import uniffi.lakko_ffi_uniffi.FfiChannelPostContent
import uniffi.lakko_ffi_uniffi.FfiChannelPostKind

/**
 * ADR-0051 §4 — the channel metadata record on the reader and in the log,
 * the FFI-free half: the newest-record fold, the classification mapping, and
 * the shared fields riding the persisted channel list.
 */
class ChannelMetaTest {

    private val strictlyNewer: (ULong, ULong) -> Boolean = { a, b -> a > b }
    private val v1 = SharedChannelMeta("Lakko News", "Daily", 100uL)
    private val v2 = SharedChannelMeta("Lakko News", "Weekly", 200uL)

    @Test
    fun `the first record wins and a newer one replaces it`() {
        assertEquals(v1, newerChannelMeta(null, v1, strictlyNewer))
        assertEquals(v2, newerChannelMeta(v1, v2, strictlyNewer))
    }

    @Test
    fun `an older or equal-stamped record never replaces the held one`() {
        assertSame(v2, newerChannelMeta(v2, v1, strictlyNewer))
        val reissue = v2.copy(description = "Different bytes, same stamp")
        assertSame(v2, newerChannelMeta(v2, reissue, strictlyNewer))
    }

    @Test
    fun `classification maps text, record and malformed and never renders a record as text`() {
        val text = classifiedPostOf(FfiChannelPostContent(FfiChannelPostKind.TEXT, "hello".toByteArray(), null, null))
        assertEquals(ClassifiedPost.Text("hello"), text)
        val meta = classifiedPostOf(
            FfiChannelPostContent(FfiChannelPostKind.META, null, FfiChannelMeta("Lakko News", "Daily", 100uL), null),
        )
        assertEquals(ClassifiedPost.Meta(v1), meta)
        assertEquals(ClassifiedPost.Malformed, classifiedPostOf(FfiChannelPostContent(FfiChannelPostKind.MALFORMED, null, null, "name.empty")))
        // A META kind with no record is a contract violation and is dropped rather than rendered.
        assertEquals(ClassifiedPost.Malformed, classifiedPostOf(FfiChannelPostContent(FfiChannelPostKind.META, null, null, null)))
        assertEquals(ClassifiedPost.Text(""), classifiedPostOf(FfiChannelPostContent(FfiChannelPostKind.TEXT, null, null, null)))
    }

    @Test
    fun `a channel holds a shared record only once one has landed`() {
        val bare = ChannelHandle(id = "ab".repeat(16), localName = "local", localDescription = "", addedAtMs = 1uL, handle = "h")
        assertNull(bare.sharedMeta())
        val withRecord = bare.copy(sharedName = "Lakko News", sharedDescription = "Daily", sharedUpdatedAtMs = 100uL)
        assertEquals(v1, withRecord.sharedMeta())
        // A stamp with a blank name is not a record.
        assertNull(bare.copy(sharedUpdatedAtMs = 5uL).sharedMeta())
    }

    @Test
    fun `the shared record rides the persisted channel list and an old row reads as none`() {
        val rows = listOf(
            ChannelHandle(id = "01".repeat(16), localName = "A", localDescription = "", addedAtMs = 10uL, handle = "a"),
            ChannelHandle(
                id = "02".repeat(16), localName = "B", localDescription = "b", addedAtMs = 20uL, handle = "",
                sharedName = "Bee", sharedDescription = "Buzz", sharedUpdatedAtMs = ULong.MAX_VALUE,
            ),
        )
        assertEquals(rows, decodeChannels(encodeChannels(rows)))
        // A row written before the record existed has no shared fields at all.
        val legacy = """[{"id":"${"03".repeat(16)}","localName":"C","localDescription":"","addedAtMs":"30","handle":"c"}]"""
        val decoded = decodeChannels(legacy)!!.single()
        assertEquals("", decoded.sharedName)
        assertNull(decoded.sharedUpdatedAtMs)
        assertNull(decoded.sharedMeta())
    }
}
