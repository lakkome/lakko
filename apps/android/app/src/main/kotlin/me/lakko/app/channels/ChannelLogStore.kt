// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.SharedPreferences
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import me.lakko.app.auth.base64Decode
import me.lakko.app.auth.base64Encode

/**
 * On-device persistence for one account's CHANNEL LOG: the channel list
 * (ids, local names, the `@handle` each was found or claimed by), the
 * subscriber-side `epoch -> content-key seed` map (ADR-0031), and the
 * per-channel rotation heartbeat state (C-54).
 *
 * Before this store all three lived in memory only, so a relaunch emptied the
 * Channels tab: a subscriber lost the list AND the seeds it had opened (the
 * server retains only the last N=4 envelopes, so an older post window was
 * gone for good), and a creator lost the list too. The subscriber keypairs,
 * the publisher signing seeds and the publish counters were already persisted
 * by [ChannelsUiState]; this is the missing piece and leaves those untouched.
 *
 * Keyed per account like the stranger-inbox store
 * (`contacts/ContactsInboxStore.kt`). The production store writes into the
 * SAME `EncryptedSharedPreferences` the subscriber keypairs use
 * ([ChannelKeyStore.encryptedPrefs], Keystore-backed AES-256-GCM): the seeds
 * are content-key material, so they are never written to plaintext prefs
 * (invariant #7). The list and the rotation state are non-secret metadata
 * that ride along in the same file for simplicity.
 *
 * The handle is persisted because handle -> id is a one-way discovery OPRF
 * ([ChannelDiscovery]); the id alone cannot be turned back into the handle.
 *
 * Keys:
 *   * `lakko.channels.list.v1:<accountIdB64>`
 *   * `lakko.channels.subscribed-seeds.v1:<accountIdB64>`
 *   * `lakko.channels.rotation-state.v1:<accountIdB64>:<channelId>`
 */
public interface ChannelLogStore {
    /** The persisted channel list, newest-first as saved; empty on first use
     *  or a decode failure (a forward-compat / corruption guard). */
    public fun loadChannels(accountIdB64: String): List<ChannelHandle>

    /** Persist the whole list as-is (an empty list is a valid state). */
    public fun saveChannels(accountIdB64: String, channels: List<ChannelHandle>)

    /** The persisted `channelId -> (epoch -> seed)` map; empty when absent. */
    public fun loadSubscribedSeeds(accountIdB64: String): Map<String, Map<UInt, ByteArray>>

    /** Persist the whole seed map as-is. */
    public fun saveSubscribedSeeds(accountIdB64: String, seeds: Map<String, Map<UInt, ByteArray>>)

    /** The persisted rotation state for one channel, or `null` when none. */
    public fun loadRotationState(accountIdB64: String, channelId: String): StoredRotationState?

    /** Persist the rotation state for one channel. */
    public fun saveRotationState(accountIdB64: String, channelId: String, state: StoredRotationState)

    /** Drop the rotation state for one channel (the channel was removed). */
    public fun deleteRotationState(accountIdB64: String, channelId: String)

    /** Drop every entry for [accountIdB64] (forget device). */
    public fun delete(accountIdB64: String)
}

/**
 * The rotation heartbeat state as stored — a plain mirror of the FFI
 * `ChannelRotationState` record so the codec and its tests need no native
 * library. The screen converts at the boundary.
 */
public data class StoredRotationState(
    /** Unix milliseconds of the last completed rotation; `0` = never. */
    val lastRotationMs: ULong,
    /** Posts published since that rotation. */
    val postsSinceRotation: UInt,
)

/** The production store over the encrypted channel [SharedPreferences]. */
public class SharedPreferencesChannelLogStore(
    private val prefs: SharedPreferences,
) : ChannelLogStore {

    override fun loadChannels(accountIdB64: String): List<ChannelHandle> =
        prefs.getString(channelsKey(accountIdB64), null)
            ?.let { decodeChannels(it) }
            ?: emptyList()

    override fun saveChannels(accountIdB64: String, channels: List<ChannelHandle>) {
        prefs.edit().putString(channelsKey(accountIdB64), encodeChannels(channels)).apply()
    }

    override fun loadSubscribedSeeds(accountIdB64: String): Map<String, Map<UInt, ByteArray>> =
        prefs.getString(seedsKey(accountIdB64), null)
            ?.let { decodeSubscribedSeeds(it) }
            ?: emptyMap()

    override fun saveSubscribedSeeds(accountIdB64: String, seeds: Map<String, Map<UInt, ByteArray>>) {
        prefs.edit().putString(seedsKey(accountIdB64), encodeSubscribedSeeds(seeds)).apply()
    }

    override fun loadRotationState(accountIdB64: String, channelId: String): StoredRotationState? =
        prefs.getString(rotationKey(accountIdB64, channelId), null)
            ?.let { decodeRotationState(it) }

    override fun saveRotationState(accountIdB64: String, channelId: String, state: StoredRotationState) {
        prefs.edit()
            .putString(rotationKey(accountIdB64, channelId), encodeRotationState(state))
            .apply()
    }

    override fun deleteRotationState(accountIdB64: String, channelId: String) {
        prefs.edit().remove(rotationKey(accountIdB64, channelId)).apply()
    }

    override fun delete(accountIdB64: String) {
        val editor = prefs.edit()
            .remove(channelsKey(accountIdB64))
            .remove(seedsKey(accountIdB64))
        val rotationPrefix = rotationKey(accountIdB64, "")
        for (key in prefs.all.keys) {
            if (key.startsWith(rotationPrefix)) editor.remove(key)
        }
        editor.apply()
    }

    private companion object {
        fun channelsKey(accountIdB64: String) = "lakko.channels.list.v1:$accountIdB64"
        fun seedsKey(accountIdB64: String) = "lakko.channels.subscribed-seeds.v1:$accountIdB64"
        fun rotationKey(accountIdB64: String, channelId: String) =
            "lakko.channels.rotation-state.v1:$accountIdB64:$channelId"
    }
}

/** Host-testable store with the same contract, backed by the same codec so a
 *  unit test exercises the bytes the prefs store writes. */
public class InMemoryChannelLogStore : ChannelLogStore {
    private val channels = HashMap<String, String>()
    private val seeds = HashMap<String, String>()
    private val rotation = HashMap<Pair<String, String>, String>()

    override fun loadChannels(accountIdB64: String): List<ChannelHandle> = synchronized(this) {
        channels[accountIdB64]?.let { decodeChannels(it) } ?: emptyList()
    }

    override fun saveChannels(accountIdB64: String, channels: List<ChannelHandle>): Unit =
        synchronized(this) {
            this.channels[accountIdB64] = encodeChannels(channels)
        }

    override fun loadSubscribedSeeds(accountIdB64: String): Map<String, Map<UInt, ByteArray>> =
        synchronized(this) {
            seeds[accountIdB64]?.let { decodeSubscribedSeeds(it) } ?: emptyMap()
        }

    override fun saveSubscribedSeeds(accountIdB64: String, seeds: Map<String, Map<UInt, ByteArray>>): Unit =
        synchronized(this) {
            this.seeds[accountIdB64] = encodeSubscribedSeeds(seeds)
        }

    override fun loadRotationState(accountIdB64: String, channelId: String): StoredRotationState? =
        synchronized(this) {
            rotation[accountIdB64 to channelId]?.let { decodeRotationState(it) }
        }

    override fun saveRotationState(accountIdB64: String, channelId: String, state: StoredRotationState): Unit =
        synchronized(this) {
            rotation[accountIdB64 to channelId] = encodeRotationState(state)
        }

    override fun deleteRotationState(accountIdB64: String, channelId: String): Unit =
        synchronized(this) {
            rotation.remove(accountIdB64 to channelId)
        }

    override fun delete(accountIdB64: String): Unit = synchronized(this) {
        channels.remove(accountIdB64)
        seeds.remove(accountIdB64)
        rotation.keys.removeAll { it.first == accountIdB64 }
    }
}

// =============================================================================
// Pure rules — no Android dependency, unit-tested on the host JVM.
// =============================================================================

/**
 * The channels whose publisher keys a relaunched process re-opens: those in
 * [channels] that have a persisted publisher signing seed (this device
 * published to them — created, or joined as a co-publisher) and that do not
 * already hold live keys. Subscriber-only channels have no signing seed and
 * are skipped; a signing seed with no matching channel row (the row was
 * removed) is skipped too. Order is the list order, so the newest channel
 * re-opens first.
 */
public fun channelsToReopen(
    channels: List<ChannelHandle>,
    signingSeedByChannel: Map<String, ByteArray>,
    channelsWithKeys: Set<String>,
): List<ChannelHandle> =
    channels.filter { it.id in signingSeedByChannel && it.id !in channelsWithKeys }

// =============================================================================
// Codec — JSON via kotlinx.serialization so it runs on the host JVM (the
// `org.json` classes are stubs there). Unsigned 64-bit values ride as decimal
// strings, like the publish counters; a seed is base64.
// =============================================================================

private val channelLogJson = Json { ignoreUnknownKeys = true }

/** ADR-0059 — an ISO 639 code of 2-3 lowercase letters, or "" (not set). */
private val LANGUAGE_OR_EMPTY = Regex("^([a-z]{2,3})?$")

@Serializable
private data class StoredChannel(
    val id: String,
    val localName: String,
    val localDescription: String,
    val addedAtMs: String,
    val handle: String = "",
    /** ADR-0051 — the shared record; absent on rows written before it existed. */
    val sharedName: String = "",
    val sharedDescription: String = "",
    /** The record stamp as text; "" when no record has landed. */
    val sharedUpdatedAtMs: String = "",
    /** ADR-0059 metadata v2; absent on rows written before it. */
    val sharedTopic: Int = 0,
    val sharedLanguage: String = "",
    val sharedListed: Boolean = false,
    /** ADR-0087 section 18.2 — the newest record's plaintext; absent on older rows. */
    val sharedRecordB64: String = "",
)

@Serializable
private data class StoredSeed(
    val epoch: Long,
    val seedB64: String,
)

@Serializable
private data class StoredChannelSeeds(
    val channelId: String,
    val seeds: List<StoredSeed>,
)

@Serializable
private data class StoredRotation(
    val lastRotationMs: String,
    val postsSinceRotation: Long,
)

internal fun encodeChannels(channels: List<ChannelHandle>): String =
    channelLogJson.encodeToString(
        channels.map {
            StoredChannel(
                id = it.id,
                localName = it.localName,
                localDescription = it.localDescription,
                addedAtMs = it.addedAtMs.toString(),
                handle = it.handle,
                sharedName = it.sharedName,
                sharedDescription = it.sharedDescription,
                sharedUpdatedAtMs = it.sharedUpdatedAtMs?.toString() ?: "",
                sharedTopic = it.sharedTopic,
                sharedLanguage = it.sharedLanguage,
                sharedListed = it.sharedListed,
                sharedRecordB64 = it.sharedRecordB64,
            )
        },
    )

/** Inverse of [encodeChannels]; `null` when [raw] does not decode. A row
 *  with an empty id or an unparseable timestamp is skipped, not fatal. */
internal fun decodeChannels(raw: String): List<ChannelHandle>? =
    try {
        channelLogJson.decodeFromString<List<StoredChannel>>(raw).mapNotNull { s ->
            if (s.id.isEmpty()) return@mapNotNull null
            val addedAt = s.addedAtMs.toULongOrNull() ?: return@mapNotNull null
            ChannelHandle(
                id = s.id,
                localName = s.localName,
                localDescription = s.localDescription,
                addedAtMs = addedAt,
                handle = s.handle,
                sharedName = s.sharedName,
                sharedDescription = s.sharedDescription,
                // A blank or unparseable stamp reads as "no record": the
                // channel falls back to its handle until one lands.
                sharedUpdatedAtMs = s.sharedUpdatedAtMs.toULongOrNull(),
                sharedTopic = s.sharedTopic.takeIf { it in 0..255 } ?: 0,
                sharedLanguage = s.sharedLanguage.takeIf { LANGUAGE_OR_EMPTY.matches(it) } ?: "",
                sharedListed = s.sharedListed,
                sharedRecordB64 = s.sharedRecordB64,
            )
        }
    } catch (_: Exception) {
        null
    }

internal fun encodeSubscribedSeeds(seeds: Map<String, Map<UInt, ByteArray>>): String =
    channelLogJson.encodeToString(
        seeds.map { (channelId, byEpoch) ->
            StoredChannelSeeds(
                channelId = channelId,
                seeds = byEpoch.entries
                    .sortedBy { it.key }
                    .map { (epoch, seed) -> StoredSeed(epoch = epoch.toLong(), seedB64 = base64Encode(seed)) },
            )
        },
    )

/** Inverse of [encodeSubscribedSeeds]; `null` when [raw] does not decode.
 *  A seed that is not a 32-byte value, or an epoch outside `UInt`, is
 *  skipped: a wrong seed is never handed to the AEAD (invariant #4). */
internal fun decodeSubscribedSeeds(raw: String): Map<String, Map<UInt, ByteArray>>? =
    try {
        val out = LinkedHashMap<String, Map<UInt, ByteArray>>()
        for (row in channelLogJson.decodeFromString<List<StoredChannelSeeds>>(raw)) {
            if (row.channelId.isEmpty()) continue
            val byEpoch = LinkedHashMap<UInt, ByteArray>()
            for (s in row.seeds) {
                if (s.epoch < 0L || s.epoch > UInt.MAX_VALUE.toLong()) continue
                val seed = try {
                    base64Decode(s.seedB64)
                } catch (_: IllegalArgumentException) {
                    continue
                }
                if (seed.size != CHANNEL_SEED_LEN) continue
                byEpoch[s.epoch.toUInt()] = seed
            }
            if (byEpoch.isNotEmpty()) out[row.channelId] = byEpoch
        }
        out
    } catch (_: Exception) {
        null
    }

internal fun encodeRotationState(state: StoredRotationState): String =
    channelLogJson.encodeToString(
        StoredRotation(
            lastRotationMs = state.lastRotationMs.toString(),
            postsSinceRotation = state.postsSinceRotation.toLong(),
        ),
    )

/** Inverse of [encodeRotationState]; `null` when [raw] does not decode or a
 *  field is out of range (the caller genesis-seeds a fresh state). */
internal fun decodeRotationState(raw: String): StoredRotationState? =
    try {
        val s = channelLogJson.decodeFromString<StoredRotation>(raw)
        val last = s.lastRotationMs.toULongOrNull()
        if (last == null || s.postsSinceRotation < 0L || s.postsSinceRotation > UInt.MAX_VALUE.toLong()) {
            null
        } else {
            StoredRotationState(lastRotationMs = last, postsSinceRotation = s.postsSinceRotation.toUInt())
        }
    } catch (_: Exception) {
        null
    }

/** Byte length of a channel content-key seed (the MLS exporter output). */
private const val CHANNEL_SEED_LEN: Int = 32
