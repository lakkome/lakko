// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.devices.sync

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * The cross-device sync state, written to this device's disk.
 *
 * Drafts, pin / mute / archive, read cursors and the chat order travel
 * between a person's devices over the DeviceGroup journal, and until this
 * existed they travelled nowhere else: the four maps were [kotlinx.coroutines.flow.StateFlow]s
 * and nothing more, so a relaunch started them empty.
 *
 * The journal cannot refill them. This client polls from cursor 0 on every
 * launch, which looks like it would - but it restores the DeviceGroup MLS
 * snapshot first, so every row it re-fetches is one its own state has already
 * consumed and cannot decrypt a second time. It re-downloads the whole
 * journal on each launch in order to fail on all of it.
 *
 * What is stored is a SNAPSHOT of the folded state, not the journal. That is
 * what makes restoring it safe: every entry carries the HLC of the payload
 * that produced it, the poll folds newer rows on top through the same
 * `shouldReplace` comparison it always used, and a sibling device's later
 * write therefore wins over a restored one exactly as it would over a live
 * one. Nothing here is authoritative; it is a head start.
 *
 * Pure encode / decode, no I/O, so a unit test can run it. The bytes are
 * sealed by `DeviceGroupSnapshotStore` under the Keystore wrap key, because a
 * draft is a message the person has not sent yet.
 *
 * The wire shape is the web client's byte for byte
 * (`devices/sync/sync-state-store.ts`) and the iOS client's. These blobs are
 * never exchanged - each device writes its own - but one description fitting
 * all three is worth more than a shape chosen three times.
 */
public object DeviceGroupSyncStateCodec {

    /** Payload schema version this client writes. */
    public const val VERSION: Int = 1

    /** The four maps, as one snapshot. */
    public data class Snapshot(
        val drafts: Map<String, DraftState> = emptyMap(),
        val flags: Map<String, FlagsState> = emptyMap(),
        val readCursors: Map<String, ReadCursorState> = emptyMap(),
        val order: OrderState? = null,
    ) {
        /** Whether this holds anything worth writing. */
        val isEmpty: Boolean
            get() = drafts.isEmpty() && flags.isEmpty() && readCursors.isEmpty() && order == null
    }

    // ----- wire shape ------------------------------------------------
    //
    // `ByteArray` and `ULong` ride as base64 and as decimal strings: a JSON
    // number does not carry a ULong near its top, and the HLC is exactly what
    // decides whose write wins.

    @Serializable
    private data class WireDraft(val t: String, val r: String, val h: String)

    @Serializable
    private data class WireFlags(
        val p: Boolean,
        val m: Boolean,
        val a: Boolean,
        val u: Boolean,
        val h: String,
    )

    @Serializable
    private data class WireCursor(val i: String, val r: String, val h: String)

    @Serializable
    private data class WireOrder(val g: List<String>, val h: String)

    @Serializable
    private data class WirePayload(
        val v: Int,
        val d: Map<String, WireDraft> = emptyMap(),
        val f: Map<String, WireFlags> = emptyMap(),
        val c: Map<String, WireCursor> = emptyMap(),
        val o: WireOrder? = null,
    )

    private val json = Json { ignoreUnknownKeys = true }

    // `java.util.Base64`, not `android.util.Base64`: the same standard
    // alphabet, and it is real on the JVM, so this codec can be run by a unit
    // test rather than only on a device. The same reason `MessageLogStore`
    // gives for the same choice.
    private val b64Encoder = java.util.Base64.getEncoder()
    private val b64Decoder = java.util.Base64.getDecoder()

    private fun b64(bytes: ByteArray): String = b64Encoder.encodeToString(bytes)

    private fun unb64(text: String): ByteArray? =
        runCatching { b64Decoder.decode(text) }.getOrNull()

    // ----- public API ------------------------------------------------

    /** Encode the snapshot. `null` only if encoding itself fails. */
    public fun encode(snapshot: Snapshot): ByteArray? = runCatching {
        json.encodeToString(
            WirePayload.serializer(),
            WirePayload(
                v = VERSION,
                d = snapshot.drafts.mapValues {
                    WireDraft(
                        t = it.value.text,
                        r = b64(it.value.replyToId),
                        h = it.value.updatedAtHlc.toString(),
                    )
                },
                f = snapshot.flags.mapValues {
                    WireFlags(
                        p = it.value.pinned,
                        m = it.value.muted,
                        a = it.value.archived,
                        u = it.value.unreadOverride,
                        h = it.value.updatedAtHlc.toString(),
                    )
                },
                c = snapshot.readCursors.mapValues {
                    WireCursor(
                        i = b64(it.value.upToMessageId),
                        r = it.value.readAtMs.toString(),
                        h = it.value.updatedAtHlc.toString(),
                    )
                },
                o = snapshot.order?.let { o ->
                    WireOrder(g = o.groupIds.map { b64(it) }, h = o.updatedAtHlc.toString())
                },
            ),
        ).toByteArray(Charsets.UTF_8)
    }.getOrNull()

    /**
     * Decode a payload.
     *
     * A malformed ENTRY is dropped and the rest of the snapshot is kept:
     * losing one chat's pin is better than losing every chat's, and the poll
     * can refill any single entry from a sibling's next write. Bytes that are
     * not this schema at all return `null`, which the caller treats as
     * "nothing stored" - the state a first run is in.
     */
    public fun decode(bytes: ByteArray): Snapshot? {
        val payload = runCatching {
            json.decodeFromString(WirePayload.serializer(), bytes.toString(Charsets.UTF_8))
        }.getOrNull() ?: return null
        if (payload.v != VERSION) return null

        val drafts = payload.d.mapNotNull { (hex, raw) ->
            val hlc = raw.h.toULongOrNull() ?: return@mapNotNull null
            val reply = unb64(raw.r) ?: return@mapNotNull null
            hex to DraftState(text = raw.t, replyToId = reply, updatedAtHlc = hlc)
        }.toMap()

        val flags = payload.f.mapNotNull { (hex, raw) ->
            val hlc = raw.h.toULongOrNull() ?: return@mapNotNull null
            hex to FlagsState(
                pinned = raw.p,
                muted = raw.m,
                archived = raw.a,
                unreadOverride = raw.u,
                updatedAtHlc = hlc,
            )
        }.toMap()

        val cursors = payload.c.mapNotNull { (hex, raw) ->
            val hlc = raw.h.toULongOrNull() ?: return@mapNotNull null
            val readAt = raw.r.toULongOrNull() ?: return@mapNotNull null
            val upTo = unb64(raw.i) ?: return@mapNotNull null
            hex to ReadCursorState(
                upToMessageId = upTo,
                readAtMs = readAt,
                updatedAtHlc = hlc,
            )
        }.toMap()

        // The order is one value rather than a map, so a single bad id would
        // reorder the chat list silently. Either all of it decodes or none
        // does.
        val order = payload.o?.let { raw ->
            val hlc = raw.h.toULongOrNull() ?: return@let null
            val ids = raw.g.map { unb64(it) }
            if (ids.any { it == null }) null
            else OrderState(groupIds = ids.filterNotNull(), updatedAtHlc = hlc)
        }

        return Snapshot(drafts = drafts, flags = flags, readCursors = cursors, order = order)
    }
}
