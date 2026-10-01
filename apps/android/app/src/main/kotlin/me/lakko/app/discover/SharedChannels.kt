// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.discover

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/**
 * ADR-0059 section 6 — the channels group members share inside a group, and
 * the switch that shares this person's own. Mirror of
 * `apps/web/src/discover/shared-channels.ts`. All pure:
 *
 *   * [looksLikeSharedChannelsEnvelope] recognises the envelope in an
 *     application plaintext BEFORE the chat parser (whose lossy-UTF-8
 *     fallback would render it as a bubble reading the domain string and the
 *     channel names);
 *   * the row this device keeps ([SharedChannelsRow]): the newest list per
 *     member per group, keyed on the member's ACCOUNT key resolved at receipt
 *     (C-68), replaced only by a strictly newer stamp; the groups the switch is
 *     on in; and what this device last sent to each, so a growth re-send
 *     carries the original stamp (the ADR-0044 lesson);
 *   * [channelsToShare]: only subscribed channels whose own metadata says
 *     `listed`, with a handle, at most [SHARED_CHANNELS_MAX].
 */

/** Most channels one list carries (`lakko-contacts::shared_channels`). */
public const val SHARED_CHANNELS_MAX: Int = 32

/**
 * The frozen domain separator, `lakko/shared-channels/v1` followed by one NUL
 * byte. Built programmatically: a raw NUL in a source file makes git treat it
 * as binary.
 */
internal val SHARED_CHANNELS_ENVELOPE_DOMAIN: ByteArray =
    "lakko/shared-channels/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

/**
 * Does this application plaintext open with the shared-channels domain? As with
 * the pin and profile peeks this is a complete CLASSIFICATION: the separator
 * ends in NUL, so no chat envelope and no text a person typed can open with it.
 * One that does and does not decode is DROPPED, never handed to the chat
 * parser. `internal` so the background preview filter uses this exact test.
 */
internal fun looksLikeSharedChannelsEnvelope(bytes: ByteArray): Boolean {
    if (bytes.size < SHARED_CHANNELS_ENVELOPE_DOMAIN.size) return false
    for (i in SHARED_CHANNELS_ENVELOPE_DOMAIN.indices) {
        if (bytes[i] != SHARED_CHANNELS_ENVELOPE_DOMAIN[i]) return false
    }
    return true
}

/** One shared channel. */
public data class SharedChannel(
    /** 32-char lowercase hex. */
    val channelIdHex: String,
    val handle: String,
    val name: String,
)

/** One member's list. */
public data class SharedList(
    val updatedAtMs: ULong,
    val channels: List<SharedChannel>,
)

/** The row this device keeps. */
public data class SharedChannelsRow(
    /** groupIdHex -> member account key (base64) -> newest list. */
    val received: Map<String, Map<String, SharedList>> = emptyMap(),
    /** groupIdHex -> the last list this device sent there. */
    val sent: Map<String, SharedList> = emptyMap(),
    /** Groups the "Share my channels" switch is on in. */
    val shareOn: Set<String> = emptySet(),
)

/** The verdict on an incoming list. */
public enum class SharedChannelsVerdict { ACCEPT, REJECT_STALE, REJECT_UNATTRIBUTED }

/** The row after a verdict. */
public data class SharedChannelsApply(val row: SharedChannelsRow, val verdict: SharedChannelsVerdict)

/**
 * File [incoming] from [memberKeyB64] in [groupIdHex] when it is strictly
 * newer than what is held for that member (the core's `sharedChannelsIsNewer`,
 * threaded in). An unattributed sender (no account key resolved at receipt) is
 * refused: a list speaks only for its sender.
 */
public fun applyIncomingList(
    row: SharedChannelsRow,
    groupIdHex: String,
    memberKeyB64: String?,
    incoming: SharedList,
    isNewer: (incoming: ULong, stored: ULong) -> Boolean,
): SharedChannelsApply {
    if (memberKeyB64.isNullOrEmpty()) return SharedChannelsApply(row, SharedChannelsVerdict.REJECT_UNATTRIBUTED)
    val group = row.received[groupIdHex]
    val held = group?.get(memberKeyB64)
    if (held != null && !isNewer(incoming.updatedAtMs, held.updatedAtMs)) {
        return SharedChannelsApply(row, SharedChannelsVerdict.REJECT_STALE)
    }
    val nextGroup = (group ?: emptyMap()) + (memberKeyB64 to incoming)
    return SharedChannelsApply(row.copy(received = row.received + (groupIdHex to nextGroup)), SharedChannelsVerdict.ACCEPT)
}

/** The group was deleted from this device: forget everything about it. */
public fun forgetGroup(row: SharedChannelsRow, groupIdHex: String): SharedChannelsRow =
    SharedChannelsRow(
        received = row.received - groupIdHex,
        sent = row.sent - groupIdHex,
        shareOn = row.shareOn - groupIdHex,
    )

/** A stamp strictly after [previous] (a clock that went backwards still
 *  produces a list every member takes as newer). */
public fun freshListStamp(nowMs: Long, previous: SharedList?): ULong {
    val now = nowMs.coerceAtLeast(0L).toULong()
    if (previous == null || now > previous.updatedAtMs) return now
    return previous.updatedAtMs + 1uL
}

/** Whether two lists name the same channels, handles and names in order. */
public fun sameChannels(a: List<SharedChannel>, b: List<SharedChannel>): Boolean = a == b

/** What the share decision reads about one channel on this device. */
public data class ShareableChannel(
    val channelIdHex: String,
    /** This device follows it as a subscriber. */
    val subscribed: Boolean,
    /** The channel's own metadata v2 says `listed`. */
    val listed: Boolean,
    /** The handle it was found under; `null` when none. */
    val handle: String?,
    /** The name the channel's metadata carries (or the local name). */
    val name: String,
)

private val HANDLE_RE = Regex("^[a-z0-9_]{3,32}$")

/**
 * The list to share: subscribed, listed, with a canonical handle and a name,
 * in channel-id order (so an unchanged set is an unchanged list), at most
 * [SHARED_CHANNELS_MAX]. A channel reached through a private handle is never
 * listed, so never shared.
 */
public fun channelsToShare(channels: List<ShareableChannel>): List<SharedChannel> =
    channels
        .filter { it.subscribed && it.listed && it.handle != null && HANDLE_RE.matches(it.handle) }
        .filter { it.name.trim().isNotEmpty() }
        .map { SharedChannel(it.channelIdHex, it.handle!!, it.name.trim()) }
        .sortedBy { it.channelIdHex }
        .take(SHARED_CHANNELS_MAX)

/** One channel as the members of the person's groups share it. */
public data class GroupShared(
    val channelIdHex: String,
    val handle: String,
    val name: String,
    /** groupIdHex -> the member keys sharing it there. */
    val byGroup: Map<String, List<String>>,
)

/**
 * Every channel shared with this person, across groups. The person's own key
 * is skipped, and so is a group this device no longer holds ([liveGroups]).
 * When members disagree on a channel's handle or name, the most common claim
 * wins, then the smallest.
 */
public fun aggregateShared(row: SharedChannelsRow, ownKeyB64: String?, liveGroups: Set<String>): List<GroupShared> {
    class Entry {
        val claims = LinkedHashMap<String, Int>()
        val byGroup = LinkedHashMap<String, MutableList<String>>()
    }
    val byChannel = LinkedHashMap<String, Entry>()
    for ((groupIdHex, members) in row.received) {
        if (groupIdHex !in liveGroups) continue
        for ((memberKey, list) in members) {
            if (memberKey == ownKeyB64) continue
            for (c in list.channels) {
                val entry = byChannel.getOrPut(c.channelIdHex) { Entry() }
                val claim = "${c.handle}\n${c.name}"
                entry.claims[claim] = (entry.claims[claim] ?: 0) + 1
                val keys = entry.byGroup.getOrPut(groupIdHex) { mutableListOf() }
                if (memberKey !in keys) keys += memberKey
            }
        }
    }
    return byChannel.map { (id, e) ->
        val claim = e.claims.entries
            .sortedWith(compareByDescending<Map.Entry<String, Int>> { it.value }.thenBy { it.key })
            .firstOrNull()?.key ?: "\n"
        val handle = claim.substringBefore('\n')
        val name = claim.substringAfter('\n')
        GroupShared(id, handle, name, e.byGroup.mapValues { it.value.toList() })
    }.sortedBy { it.channelIdHex }
}

// ----- Row codec (JSON, sealed by the store) --------------------------------

@Serializable
private data class WireList(val u: String, val c: List<List<String>>)

@Serializable
private data class WireRow(
    val version: Int = 0,
    val received: Map<String, Map<String, WireList>> = emptyMap(),
    val sent: Map<String, WireList> = emptyMap(),
    val shareOn: List<String> = emptyList(),
)

private val rowJson = Json { ignoreUnknownKeys = true }
private val HEX_ID = Regex("^[0-9a-f]{32}$")
private val STAMP = Regex("^\\d{1,20}$")

private fun encodeList(l: SharedList): WireList =
    WireList(l.updatedAtMs.toString(), l.channels.map { listOf(it.channelIdHex, it.handle, it.name) })

private fun decodeList(w: WireList): SharedList? {
    if (!STAMP.matches(w.u)) return null
    val stamp = w.u.toULongOrNull() ?: return null
    val channels = mutableListOf<SharedChannel>()
    for (c in w.c) {
        if (c.size != 3 || !HEX_ID.matches(c[0])) continue
        channels += SharedChannel(c[0], c[1], c[2])
        if (channels.size >= SHARED_CHANNELS_MAX) break
    }
    return SharedList(stamp, channels)
}

public fun encodeSharedChannelsRow(row: SharedChannelsRow): String =
    rowJson.encodeToString(
        WireRow.serializer(),
        WireRow(
            version = 1,
            received = row.received.mapValues { (_, m) -> m.mapValues { encodeList(it.value) } },
            sent = row.sent.mapValues { encodeList(it.value) },
            shareOn = row.shareOn.toList(),
        ),
    )

/** Lenient: anything that does not parse reads as an empty row. */
public fun decodeSharedChannelsRow(json: String): SharedChannelsRow {
    val w = try {
        rowJson.decodeFromString(WireRow.serializer(), json)
    } catch (_: Throwable) {
        return SharedChannelsRow()
    }
    if (w.version != 1) return SharedChannelsRow()
    return SharedChannelsRow(
        received = w.received.mapValues { (_, m) ->
            m.mapNotNull { (k, l) -> decodeList(l)?.let { k to it } }.toMap()
        },
        sent = w.sent.mapNotNull { (g, l) -> decodeList(l)?.let { g to it } }.toMap(),
        shareOn = w.shareOn.toSet(),
    )
}
