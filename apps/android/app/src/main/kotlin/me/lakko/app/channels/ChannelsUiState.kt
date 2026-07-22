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
 * persisted into the `SharedPreferences` supplied at
 * construction time so a process kill does not strand the
 * user without the secret needed to open future seed
 * envelopes. The production host passes an
 * `EncryptedSharedPreferences` (Keystore-backed AES-256-GCM,
 * `ChannelKeyStore.encryptedPrefs`), so the secret half is
 * encrypted at rest (G-29, invariant #7), never plaintext;
 * `null` (the test default) disables persistence.
 *
 * Publisher **signing** seeds are persisted the same way into
 * [publisherPrefs] (`ChannelPublisherKeyStore.encryptedPrefs`)
 * so a relaunched publisher keeps signing its channels under
 * the same Ed25519 key (G-29); `null` disables persistence.
 * The content-key `seed` re-derives from the MLS PublisherGroup
 * on open, so it is not persisted here.
 */
public class ChannelsUiState(
    private val prefs: SharedPreferences? = null,
    private val publisherPrefs: SharedPreferences? = null,
) {

    public companion object {
        /** Storage key for subscriber-keypair persistence.
         *  Same name the web + iOS sides use so an
         *  on-device audit grep finds every store at
         *  once. */
        public const val SUBSCRIBER_KEYS_PREF_KEY: String =
            "lakko.channels.subscriber-keys.v1"

        /** Storage key for publisher signing-seed persistence
         *  (G-29). Same name the iOS side keys its Keychain
         *  rows under so an on-device audit grep finds both
         *  publisher stores at once. */
        public const val PUBLISHER_SIGNING_SEEDS_PREF_KEY: String =
            "lakko.channels.publisher-signing-seeds.v1"
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

    private val _subscribedSeedsByChannel =
        MutableStateFlow<Map<String, Map<UInt, ByteArray>>>(emptyMap())
    /** Per-channel `epoch -> content-key seed` map (Phase 4b
     *  G4-3c + ADR-0031 multi-epoch retention). In-memory
     *  only — never persisted, never sent off-device. A
     *  subscriber that missed a rotation opens the server's
     *  retained set (up to N=4 epochs) into this map on
     *  catch-up, then decrypts each post with the seed
     *  matching ITS `epoch_id`. UI shows a "waiting for seed
     *  delivery" state until at least one epoch lands here. */
    public val subscribedSeedsByChannel: StateFlow<Map<String, Map<UInt, ByteArray>>> =
        _subscribedSeedsByChannel.asStateFlow()

    /** Per-channel persisted publisher **signing** seed (G-29),
     *  restored from [publisherPrefs] on construction. The
     *  publisher-open path ([ChannelsComposer.freshPublisherKeys])
     *  reuses the seed here so this device continues signing the
     *  channel under the same Ed25519 key across a relaunch.
     *  In-memory mirror of the encrypted store; never exposed as
     *  a flow (it drives the open path, not the UI). */
    private var publisherSigningSeedByChannel: Map<String, ByteArray> =
        loadPublisherSigningSeeds()

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
        // Drop this channel's persisted publisher signing seed (G-29) — the
        // device is no longer a publisher for it.
        publisherSigningSeedByChannel = publisherSigningSeedByChannel - channelId
        persistPublisherSigningSeeds()
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

    /** Install fresh publisher keys; resets counter to 0.
     *  Persists the `signingKeySeed` half to the Keystore-backed
     *  store (G-29) so a relaunch reuses it via
     *  [persistedPublisherSigningSeed]. */
    public fun setKeys(channelId: String, keys: ChannelPublisherKeys) {
        _keysByChannel.update { it + (channelId to keys) }
        _counterByChannel.update { it + (channelId to 0u) }
        publisherSigningSeedByChannel =
            publisherSigningSeedByChannel + (channelId to keys.signingKeySeed)
        persistPublisherSigningSeeds()
    }

    /** The persisted per-channel publisher signing seed, or `null`
     *  when this device has never published to [channelId]. The
     *  publisher-open path hands it to
     *  [ChannelsComposer.freshPublisherKeys] so signing continues
     *  under the same Ed25519 key across a relaunch (G-29). */
    public fun persistedPublisherSigningSeed(channelId: String): ByteArray? =
        publisherSigningSeedByChannel[channelId]

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
        _subscribedSeedsByChannel.update { it - channelId }
        persistSubscriberKeys()
    }

    /** Cache the recovered content-key seed for a specific
     *  epoch after the subscriber opened that epoch's seed
     *  envelope (ADR-0031). Idempotent per `(channel, epoch)`
     *  — re-opening the same epoch overwrites with identical
     *  bytes. */
    public fun setSubscribedSeed(channelId: String, epoch: UInt, seed: ByteArray) {
        _subscribedSeedsByChannel.update { outer ->
            val inner = outer[channelId] ?: emptyMap()
            outer + (channelId to (inner + (epoch to seed)))
        }
    }

    /** Snapshot the `epoch -> seed` map recovered so far for
     *  `channelId` (empty when nothing has been opened yet).
     *  The subscriber read loop looks up each post's epoch in
     *  this map. */
    public fun subscribedSeeds(channelId: String): Map<UInt, ByteArray> =
        _subscribedSeedsByChannel.value[channelId] ?: emptyMap()

    public fun reset() {
        _channels.value = emptyList()
        _postsByChannel.value = emptyMap()
        _epochByChannel.value = emptyMap()
        _keysByChannel.value = emptyMap()
        _counterByChannel.value = emptyMap()
        _subscriberKeysByChannel.value = emptyMap()
        _subscribedSeedsByChannel.value = emptyMap()
        publisherSigningSeedByChannel = emptyMap()
        persistSubscriberKeys()
        persistPublisherSigningSeeds()
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

    // -----------------------------------------------------------------
    // Publisher signing-seed persistence (G-29)
    // -----------------------------------------------------------------

    /** JSON array of `{channelId, signingSeedHex}` rows — the same
     *  hex-row shape the subscriber blob uses, so an on-device audit
     *  grep reads both publisher + subscriber stores identically. */
    private fun persistPublisherSigningSeeds() {
        val sink = publisherPrefs ?: return
        val arr = JSONArray()
        for ((channelId, seed) in publisherSigningSeedByChannel) {
            val row = JSONObject()
            row.put("channelId", channelId)
            row.put("signingSeedHex", ChannelsComposer.bytesToHex(seed))
            arr.put(row)
        }
        sink.edit().putString(PUBLISHER_SIGNING_SEEDS_PREF_KEY, arr.toString()).apply()
    }

    private fun loadPublisherSigningSeeds(): Map<String, ByteArray> {
        val raw = publisherPrefs?.getString(PUBLISHER_SIGNING_SEEDS_PREF_KEY, null)
            ?: return emptyMap()
        val arr = runCatching { JSONArray(raw) }.getOrNull() ?: return emptyMap()
        val out = LinkedHashMap<String, ByteArray>(arr.length())
        for (i in 0 until arr.length()) {
            val row = arr.optJSONObject(i) ?: continue
            val channelId = row.optString("channelId").takeIf { it.isNotEmpty() } ?: continue
            val seed = hexToBytes(row.optString("signingSeedHex")) ?: continue
            out[channelId] = seed
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
