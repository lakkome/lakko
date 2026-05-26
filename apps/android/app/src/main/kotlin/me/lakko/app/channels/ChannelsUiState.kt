// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.SharedPreferences
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import org.json.JSONArray
import org.json.JSONObject

/**
 * Local channels state (Phase 4 G4-2 Android). Mirror of
 * `apps/web/src/state/channels-state.ts` and
 * `apps/ios/Lakko/Channels/ChannelsUiState.swift`.
 *
 * StateFlow-backed so Compose's `collectAsState` picks
 * up every mutation without manual rebroadcast. Holds
 * the user's channel list, per-channel publisher keys,
 * server-cached posts + epochs. Display names +
 * descriptions are **local-only** — never sent to the
 * server.
 *
 * Subscriber-side X25519 keypairs (Phase 4b G4-3c) are
 * persisted into the optional `SharedPreferences`
 * supplied at construction time so a process kill does
 * not strand the user without the secret needed to open
 * future seed envelopes. A follow-up sprint moves the
 * secret half into the Android Keystore via
 * `androidx.security.crypto`.
 */
public class ChannelsUiState(
    private val prefs: SharedPreferences? = null,
) {

    public companion object {
        /** Storage key for subscriber-keypair persistence.
         *  Same name the web + iOS sides use so an
         *  on-device audit grep finds every store at
         *  once. */
        public const val SUBSCRIBER_KEYS_PREF_KEY: String =
            "lakko.channels.subscriber-keys.v1"
    }

    private val _channels = MutableStateFlow<List<ChannelHandle>>(emptyList())
    /** User's local channel list, newest-first. */
    public val channels: StateFlow<List<ChannelHandle>> = _channels.asStateFlow()

    private val _postsByChannel = MutableStateFlow<Map<String, List<PostView>>>(emptyMap())
    /** Per-channel post cache. */
    public val postsByChannel: StateFlow<Map<String, List<PostView>>> =
        _postsByChannel.asStateFlow()

    private val _epochByChannel = MutableStateFlow<Map<String, UInt>>(emptyMap())
    /** Per-channel server epoch cache. */
    public val epochByChannel: StateFlow<Map<String, UInt>> =
        _epochByChannel.asStateFlow()

    private val _keysByChannel =
        MutableStateFlow<Map<String, ChannelPublisherKeys>>(emptyMap())
    /** Per-channel publisher keys (local-only). */
    public val keysByChannel: StateFlow<Map<String, ChannelPublisherKeys>> =
        _keysByChannel.asStateFlow()

    private val _counterByChannel = MutableStateFlow<Map<String, ULong>>(emptyMap())
    /** Per-channel monotone publish counter. Reset on
     *  ``setKeys``. */
    public val counterByChannel: StateFlow<Map<String, ULong>> =
        _counterByChannel.asStateFlow()

    private val _subscriberKeysByChannel =
        MutableStateFlow<Map<String, ChannelSubscriberKeys>>(loadSubscriberKeys())
    /** Per-channel subscriber-side X25519 keypair
     *  (Phase 4b G4-3c). Persisted in SharedPreferences
     *  under ``SUBSCRIBER_KEYS_PREF_KEY`` whenever the
     *  map changes. */
    public val subscriberKeysByChannel: StateFlow<Map<String, ChannelSubscriberKeys>> =
        _subscriberKeysByChannel.asStateFlow()

    private val _subscribedSeedByChannel = MutableStateFlow<Map<String, ByteArray>>(emptyMap())
    /** Per-channel recovered content-key seed (Phase 4b
     *  G4-3c). In-memory only — never persisted, never
     *  sent off-device. UI shows a "waiting for seed
     *  delivery" state until a value lands here. */
    public val subscribedSeedByChannel: StateFlow<Map<String, ByteArray>> =
        _subscribedSeedByChannel.asStateFlow()

    public fun addChannel(handle: ChannelHandle) {
        _channels.update { list ->
            // Id-idempotent: re-adding refreshes the local
            // metadata rather than appending a duplicate.
            val without = list.filter { it.id != handle.id }
            listOf(handle) + without
        }
    }

    public fun removeChannel(channelId: String) {
        _channels.update { it.filter { c -> c.id != channelId } }
        _postsByChannel.update { it - channelId }
        _epochByChannel.update { it - channelId }
        _keysByChannel.update { it - channelId }
        _counterByChannel.update { it - channelId }
    }

    public fun updateLocalMeta(
        channelId: String,
        localName: String? = null,
        localDescription: String? = null,
    ) {
        _channels.update { list ->
            list.map {
                if (it.id == channelId) {
                    it.copy(
                        localName = localName ?: it.localName,
                        localDescription = localDescription ?: it.localDescription,
                    )
                } else {
                    it
                }
            }
        }
    }

    public fun setPosts(channelId: String, posts: List<PostView>) {
        _postsByChannel.update { it + (channelId to posts) }
    }

    public fun setEpoch(channelId: String, epoch: UInt) {
        _epochByChannel.update { it + (channelId to epoch) }
    }

    /** Install fresh publisher keys; resets counter to 0. */
    public fun setKeys(channelId: String, keys: ChannelPublisherKeys) {
        _keysByChannel.update { it + (channelId to keys) }
        _counterByChannel.update { it + (channelId to 0u) }
    }

    /** Atomically read-and-bump the per-channel publish
     *  counter. Returns the value the NEXT post should
     *  carry. */
    public fun nextCounter(channelId: String): ULong {
        var value: ULong = 0u
        _counterByChannel.update { map ->
            value = map[channelId] ?: 0u
            map + (channelId to (value + 1u))
        }
        return value
    }

    /** Install / replace the subscriber-side X25519
     *  keypair the device generated when joining
     *  `channelId` as a reader. Persists synchronously. */
    public fun setSubscriberKeys(channelId: String, keys: ChannelSubscriberKeys) {
        _subscriberKeysByChannel.update { it + (channelId to keys) }
        persistSubscriberKeys()
    }

    /** Forget the subscriber-side keypair for
     *  `channelId` — used on un-follow. Clears the cached
     *  recovered seed too. */
    public fun clearSubscriberKeys(channelId: String) {
        _subscriberKeysByChannel.update { it - channelId }
        _subscribedSeedByChannel.update { it - channelId }
        persistSubscriberKeys()
    }

    /** Cache the recovered content-key seed after the
     *  subscriber successfully opened a seed envelope. */
    public fun setSubscribedSeed(channelId: String, seed: ByteArray) {
        _subscribedSeedByChannel.update { it + (channelId to seed) }
    }

    public fun reset() {
        _channels.value = emptyList()
        _postsByChannel.value = emptyMap()
        _epochByChannel.value = emptyMap()
        _keysByChannel.value = emptyMap()
        _counterByChannel.value = emptyMap()
        _subscriberKeysByChannel.value = emptyMap()
        _subscribedSeedByChannel.value = emptyMap()
        persistSubscriberKeys()
    }

    // -----------------------------------------------------------------
    // Subscriber-keypair persistence
    // -----------------------------------------------------------------

    /** JSON array of `{channelId, secretHex, publicHex}`
     *  rows — same shape the web localStorage + iOS
     *  UserDefaults stores carry, so a future encrypted-
     *  device-group-keyed cache can import all three at
     *  once. */
    private fun persistSubscriberKeys() {
        val sink = prefs ?: return
        val arr = JSONArray()
        for ((channelId, keys) in _subscriberKeysByChannel.value) {
            val row = JSONObject()
            row.put("channelId", channelId)
            row.put("secretHex", ChannelsComposer.bytesToHex(keys.secret))
            row.put("publicHex", ChannelsComposer.bytesToHex(keys.publicKey))
            arr.put(row)
        }
        sink.edit().putString(SUBSCRIBER_KEYS_PREF_KEY, arr.toString()).apply()
    }

    private fun loadSubscriberKeys(): Map<String, ChannelSubscriberKeys> {
        val raw = prefs?.getString(SUBSCRIBER_KEYS_PREF_KEY, null) ?: return emptyMap()
        val arr = runCatching { JSONArray(raw) }.getOrNull() ?: return emptyMap()
        val out = LinkedHashMap<String, ChannelSubscriberKeys>(arr.length())
        for (i in 0 until arr.length()) {
            val row = arr.optJSONObject(i) ?: continue
            val channelId = row.optString("channelId").takeIf { it.isNotEmpty() } ?: continue
            val secretHex = row.optString("secretHex")
            val publicHex = row.optString("publicHex")
            val secret = hexToBytes(secretHex) ?: continue
            val pub = hexToBytes(publicHex) ?: continue
            out[channelId] = ChannelSubscriberKeys(secret = secret, publicKey = pub)
        }
        return out
    }

    private fun hexToBytes(hex: String): ByteArray? {
        if (hex.length % 2 != 0) return null
        val out = ByteArray(hex.length / 2)
        for (i in out.indices) {
            val byte = hex.substring(i * 2, i * 2 + 2).toIntOrNull(16) ?: return null
            out[i] = byte.toByte()
        }
        return out
    }
}

/** One channel as the local UI sees it. */
public data class ChannelHandle(
    /** 32-char lowercase hex (the wire form). */
    public val id: String,
    public val localName: String,
    public val localDescription: String,
    /** Unix milliseconds. */
    public val addedAtMs: ULong,
)
