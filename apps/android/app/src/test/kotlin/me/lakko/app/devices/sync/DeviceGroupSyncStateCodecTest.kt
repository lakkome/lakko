// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The cross-device sync state has to survive a relaunch.
 *
 * Drafts, pin / mute / archive, read cursors and the chat order were
 * `StateFlow`s and nothing more, so a relaunch started them empty - and the
 * journal cannot refill them: the poll runs from cursor 0, but the MLS
 * snapshot is restored first, so every row it re-fetches is one this device's
 * state has already consumed.
 */
class DeviceGroupSyncStateCodecTest {

    private val full = DeviceGroupSyncStateCodec.Snapshot(
        drafts = mapOf(
            "aa" to DraftState(
                text = "half a sentence",
                replyToId = byteArrayOf(1, 2, 3),
                updatedAtHlc = 7uL,
            ),
            "bb" to DraftState(text = "", replyToId = ByteArray(0), updatedAtHlc = 1uL),
        ),
        flags = mapOf(
            "aa" to FlagsState(
                pinned = true,
                muted = false,
                archived = false,
                unreadOverride = true,
                updatedAtHlc = 9uL,
            ),
        ),
        readCursors = mapOf(
            "aa" to ReadCursorState(
                upToMessageId = byteArrayOf(9, 9),
                readAtMs = 1_700_000_000_000uL,
                updatedAtHlc = 11uL,
            ),
        ),
        order = OrderState(
            groupIds = listOf(byteArrayOf(1), byteArrayOf(2)),
            updatedAtHlc = 13uL,
        ),
    )

    /** `ByteArray` has reference equality, so the data classes cannot be
     *  compared directly; this walks the parts that matter. */
    private fun assertSame(
        expected: DeviceGroupSyncStateCodec.Snapshot,
        actual: DeviceGroupSyncStateCodec.Snapshot?,
    ) {
        requireNotNull(actual)
        assertEquals(expected.drafts.keys, actual.drafts.keys)
        for ((k, v) in expected.drafts) {
            assertEquals(v.text, actual.drafts[k]?.text)
            assertEquals(v.updatedAtHlc, actual.drafts[k]?.updatedAtHlc)
            assertEquals(v.replyToId.toList(), actual.drafts[k]?.replyToId?.toList())
        }
        assertEquals(expected.flags, actual.flags)
        assertEquals(expected.readCursors.keys, actual.readCursors.keys)
        for ((k, v) in expected.readCursors) {
            assertEquals(v.readAtMs, actual.readCursors[k]?.readAtMs)
            assertEquals(v.updatedAtHlc, actual.readCursors[k]?.updatedAtHlc)
            assertEquals(
                v.upToMessageId.toList(),
                actual.readCursors[k]?.upToMessageId?.toList(),
            )
        }
        assertEquals(expected.order?.updatedAtHlc, actual.order?.updatedAtHlc)
        assertEquals(
            expected.order?.groupIds?.map { it.toList() },
            actual.order?.groupIds?.map { it.toList() },
        )
    }

    @Test
    fun `round-trips everything a relaunch used to lose`() {
        val blob = requireNotNull(DeviceGroupSyncStateCodec.encode(full))
        assertSame(full, DeviceGroupSyncStateCodec.decode(blob))
    }

    @Test
    fun `keeps the HLC exactly, because it is what decides who wins`() {
        // The restored snapshot is folded against the journal by HLC. A stamp
        // that came back rounded - which is what a JSON number does to a
        // ULong near its top - would let a stale sibling write overwrite a
        // newer local one.
        val big = ULong.MAX_VALUE - 1uL
        val snapshot = DeviceGroupSyncStateCodec.Snapshot(
            flags = mapOf(
                "aa" to FlagsState(
                    pinned = true,
                    muted = false,
                    archived = false,
                    unreadOverride = false,
                    updatedAtHlc = big,
                ),
            ),
        )
        val blob = requireNotNull(DeviceGroupSyncStateCodec.encode(snapshot))
        assertEquals(big, DeviceGroupSyncStateCodec.decode(blob)?.flags?.get("aa")?.updatedAtHlc)
    }

    @Test
    fun `round-trips an empty snapshot`() {
        val empty = DeviceGroupSyncStateCodec.Snapshot()
        val blob = requireNotNull(DeviceGroupSyncStateCodec.encode(empty))
        assertSame(empty, DeviceGroupSyncStateCodec.decode(blob))
        assertTrue(empty.isEmpty)
        assertFalse(full.isEmpty)
    }

    @Test
    fun `refuses bytes that are not this schema`() {
        assertNull(DeviceGroupSyncStateCodec.decode("not json".toByteArray()))
        assertNull(DeviceGroupSyncStateCodec.decode(ByteArray(0)))
        assertNull(DeviceGroupSyncStateCodec.decode("""{"v":99}""".toByteArray()))
    }

    @Test
    fun `a malformed entry is dropped without taking the rest with it`() {
        // Losing one chat's pin is better than losing every chat's, and the
        // poll can refill a single entry from a sibling's next write.
        val blob = requireNotNull(DeviceGroupSyncStateCodec.encode(full))
        val wire = Json.parseToJsonElement(blob.toString(Charsets.UTF_8)).jsonObject
        val drafts = wire["d"]!!.jsonObject
        val broken = buildJsonObject {
            for ((k, v) in drafts) {
                if (k == "aa") {
                    put(k, buildJsonObject {
                        for ((dk, dv) in v.jsonObject) {
                            put(dk, if (dk == "h") JsonPrimitive("later") else dv)
                        }
                    })
                } else {
                    put(k, v)
                }
            }
        }
        val patched = JsonObject(wire.toMutableMap().apply { put("d", broken) })
        val back = DeviceGroupSyncStateCodec.decode(patched.toString().toByteArray())
        assertNull(back?.drafts?.get("aa"))
        // The survivors, asserted too: a decoder that gave up on the whole
        // snapshot would satisfy the line above on its own.
        assertEquals("", back?.drafts?.get("bb")?.text)
        assertEquals(true, back?.flags?.get("aa")?.pinned)
    }

    @Test
    fun `drops the whole order when one id is unreadable`() {
        // The order is one value, not a map: keeping the readable ids would
        // silently reorder the chat list rather than leave it alone.
        val blob = requireNotNull(DeviceGroupSyncStateCodec.encode(full))
        val wire = Json.parseToJsonElement(blob.toString(Charsets.UTF_8)).jsonObject
        val order = wire["o"]!!.jsonObject
        val brokenOrder = buildJsonObject {
            put("g", buildJsonArray { add(JsonPrimitive("AQ==")); add(JsonPrimitive("!!!")) })
            put("h", order["h"]!!)
        }
        val patched = JsonObject(wire.toMutableMap().apply { put("o", brokenOrder) })
        val back = DeviceGroupSyncStateCodec.decode(patched.toString().toByteArray())
        assertNull(back?.order)
        assertEquals(true, back?.flags?.get("aa")?.pinned)
    }
}
