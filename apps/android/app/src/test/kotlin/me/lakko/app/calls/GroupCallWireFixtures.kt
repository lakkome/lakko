// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

/**
 * Hand-encoded `CallSignalingPayload` group-control bodies, byte for byte what
 * prost writes for `schemas/mls-app/call_signaling_v1.proto` (fields in number
 * order, zero-valued scalars omitted). Hand-encoded rather than composed through
 * the FFI because the UniFFI `.so` does not load in a host unit test — and
 * because the sniffers under test read raw bytes, so raw bytes are the honest
 * fixture.
 */
internal object GroupCallWireFixtures {

    val callId: ByteArray = ByteArray(16) { (0xC0 + it).toByte() }

    /** field 14: `GroupCallStart { call_id, initiator_leaf, created_at_ms, media_flags }`. */
    fun groupStart(leaf: Int = 3, createdAtMs: Long = 1_800_000_000_000L, mediaFlags: Int = 1): ByteArray =
        outer(
            tag = byteArrayOf(0x72),
            body = callIdField() + varintField(0x10, leaf.toLong()) +
                varintField(0x18, createdAtMs) + varintField(0x20, mediaFlags.toLong()),
        )

    /** field 15: `GroupCallJoin { call_id, leaf_index, joined_at_ms }`. */
    fun groupJoin(leaf: Int = 3, joinedAtMs: Long = 1_800_000_000_000L): ByteArray =
        outer(
            tag = byteArrayOf(0x7A),
            body = callIdField() + varintField(0x10, leaf.toLong()) + varintField(0x18, joinedAtMs),
        )

    /** field 16: `GroupCallLeave { call_id, leaf_index, reason }` — a TWO-byte tag. */
    fun groupLeave(leaf: Int = 3, reason: Int = 1): ByteArray =
        outer(
            tag = byteArrayOf(0x82.toByte(), 0x01),
            body = callIdField() + varintField(0x10, leaf.toLong()) + varintField(0x18, reason.toLong()),
        )

    private fun outer(tag: ByteArray, body: ByteArray): ByteArray = tag + varint(body.size.toLong()) + body

    private fun callIdField(): ByteArray = byteArrayOf(0x0A, callId.size.toByte()) + callId

    private fun varintField(tag: Int, value: Long): ByteArray =
        if (value == 0L) ByteArray(0) else byteArrayOf(tag.toByte()) + varint(value)

    private fun varint(value: Long): ByteArray {
        val out = ArrayList<Byte>()
        var v = value
        while (true) {
            if (v and 0x7FL.inv() == 0L) {
                out.add(v.toByte())
                return out.toByteArray()
            }
            out.add(((v and 0x7F) or 0x80).toByte())
            v = v ushr 7
        }
    }
}
