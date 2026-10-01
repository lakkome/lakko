// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.SharedPreferences
import android.util.Log
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.withContext
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
 *
 * Both, and the publish counters, are per ACCOUNT: each row is keyed
 * `<key>:<accountIdB64>` ([accountKey]), loaded on [bind] and written only
 * while an account is bound, so a second account signed in on the same phone
 * never reuses the first account's subscriber keypair or publisher key.
 *
 * The channel LIST, the subscriber `epoch -> seed` map and the per-channel
 * rotation state are persisted per account through [logStore]
 * ([ChannelLogStore], the same encrypted prefs file) from [bind] on:
 * write-through on every mutation, loaded on bind. The MLS PublisherGroups
 * live in [publisher] ([ChannelPublisherIdentity]), whose provider is
 * snapshotted so a creator's content seed survives a relaunch; [bind]
 * re-opens the publisher keys of every channel this device has a signing
 * seed for. `null` for either (the test default) disables that half.
 */
public class ChannelsUiState(
    private val prefs: SharedPreferences? = null,
    private val publisherPrefs: SharedPreferences? = null,
    private val logStore: ChannelLogStore? = null,
    /** The shared, snapshotted publisher identity every owner-side MLS call
     *  runs on; `null` disables the publisher half (tests). */
    public val publisher: ChannelPublisherIdentity? = null,
) {

    public companion object {
        private const val TAG = "ChannelsUiState"

        /** Storage key for subscriber-keypair persistence.
         *  Same name the web + iOS sides use so an
         *  on-device audit grep finds every store at
         *  once. */
        public const val SUBSCRIBER_KEYS_PREF_KEY: String =
            "lakko.channels.subscriber-keys.v1"

        /** ADR-0060 - the subscriber key scheme a persisted row was made
         *  under (Ed25519, the seed envelope's X25519 derived from it). */
        public const val SUBSCRIBER_KEY_SCHEME_ED25519: String = "ed25519"

        /** Storage key for publisher signing-seed persistence
         *  (G-29). Same name the iOS side keys its Keychain
         *  rows under so an on-device audit grep finds both
         *  publisher stores at once. */
        public const val PUBLISHER_SIGNING_SEEDS_PREF_KEY: String =
            "lakko.channels.publisher-signing-seeds.v1"

        /** Storage key for the per-channel publish **counter**
         *  persistence. The AEAD key and nonce for each post are
         *  HKDF-derived from `(seed, channel_id, epoch_id,
         *  sender_pubkey)`, the nonce also from the message
         *  counter, and both the content `seed` (re-derived from
         *  the persisted MLS PublisherGroup exporter) and the
         *  sender key (the persisted signing seed) are STABLE
         *  across a relaunch. An in-memory-only counter therefore
         *  restarts at 0 on relaunch WITHIN the same epoch + seed
         *  + sender key and reuses the same `(key, nonce)` on a
         *  new plaintext — catastrophic ChaCha20 keystream reuse.
         *  Persisting the counter at rest, per account, beside the
         *  signing seeds makes it monotone for as long as that
         *  signing key exists; `epoch_id` is in the HKDF, so no
         *  per-epoch reset is needed. The row is removed only
         *  together with the account's signing seeds
         *  ([deleteAccountRows]). */
        public const val PUBLISHER_COUNTERS_PREF_KEY: String =
            "lakko.channels.publisher-counters.v1"

        /**
         * The row of [baseKey] for one account: `<baseKey>:<accountIdB64>`
         * (standard base64). The subscriber keypairs, publisher signing seeds
         * and publish counters are the account's, not the device's; until
         * 2026-09-30 each lived under the bare [baseKey], shared by every
         * account signed in on the phone. The bare rows are never read;
         * [deleteAccountRows] removes them.
         */
        public fun accountKey(baseKey: String, accountIdB64: String): String =
            baseKey + ":" + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)

        /**
         * Forget device / Delete account: delete [accountIdB64]'s persisted
         * channel log (list, subscribed seeds, rotation state), subscriber
         * keypairs, publisher signing seeds and publish counters, and the
         * retired device-wide rows of builds before 2026-09-30. Other
         * accounts' rows stay. The one production caller is
         * [me.lakko.app.account.AccountLocalWipe]; the session state itself
         * is dropped by [unbind].
         *
         * Why the counter goes WITH the signing seeds, and only with them: a
         * post's AEAD key and nonce both derive from
         * `(seed, channel_id, epoch_id, sender_pubkey)` and the nonce also
         * from the counter (`lakko-channels` `derive_key_and_nonce`). The
         * sender key is random per device and channel
         * (`SigningKey::generate`), never derived from the recovery phrase
         * and never carried in a backup, a pairing bundle or DeviceGroup
         * sync, so once its seed is deleted here it is unrecoverable: the
         * next post on this channel signs under a NEW sender key, whose
         * `(key, nonce)` space is disjoint, and a counter restarting from 0
         * cannot collide. The one dangerous shape is a counter rewinding
         * while the SAME signing seed is still stored, which is why
         * [removeChannel] and [unbind] keep the counter, and why nothing may
         * delete the counter row without deleting the account's signing
         * seeds in the same edit.
         */
        public fun deleteAccountRows(
            prefs: SharedPreferences?,
            publisherPrefs: SharedPreferences?,
            logStore: ChannelLogStore?,
            accountIdB64: String,
        ) {
            logStore?.delete(accountIdB64)
            prefs?.edit()
                ?.remove(accountKey(SUBSCRIBER_KEYS_PREF_KEY, accountIdB64))
                ?.remove(SUBSCRIBER_KEYS_PREF_KEY)
                ?.apply()
            publisherPrefs?.edit()
                ?.remove(accountKey(PUBLISHER_SIGNING_SEEDS_PREF_KEY, accountIdB64))
                ?.remove(accountKey(PUBLISHER_COUNTERS_PREF_KEY, accountIdB64))
                ?.remove(PUBLISHER_SIGNING_SEEDS_PREF_KEY)
                ?.remove(PUBLISHER_COUNTERS_PREF_KEY)
                ?.apply()
        }

        /** Per channel, the higher of two counter floors (a counter never
         *  moves backwards, whichever side held it). */
        internal fun mergeCounterFloors(a: Map<String, ULong>, b: Map<String, ULong>): Map<String, ULong> {
            val out = HashMap(a)
            for ((channel, floor) in b) {
                val held = out[channel]
                if (held == null || floor > held) out[channel] = floor
            }
            return out
        }
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

    private val _counterByChannel =
        MutableStateFlow<Map<String, ULong>>(emptyMap())
    /** Per-channel forever-monotone publish counter, restored
     *  from [publisherPrefs] for the account on [bind] so it survives a
     *  relaunch. Seeded to 0 only for a channel with no persisted
     *  value ([setKeys] is load-else-0); NEVER reset on a key
     *  install or an epoch rotation — resetting it against the
     *  stable content seed would repeat a `(key, nonce)` pair
     *  (AEAD nonce reuse). Advanced + persisted by [nextCounter]. */
    public val counterByChannel: StateFlow<Map<String, ULong>> =
        _counterByChannel.asStateFlow()

    private val _subscriberKeysByChannel =
        MutableStateFlow<Map<String, ChannelSubscriberKeys>>(emptyMap())
    /** Per-channel subscriber-side keypair of the bound account
     *  (Phase 4b G4-3c). Persisted in SharedPreferences under
     *  ``SUBSCRIBER_KEYS_PREF_KEY:<account>`` whenever the map
     *  changes; loaded on [bind]. */
    public val subscriberKeysByChannel: StateFlow<Map<String, ChannelSubscriberKeys>> =
        _subscriberKeysByChannel.asStateFlow()

    private val _subscribedSeedsByChannel =
        MutableStateFlow<Map<String, Map<UInt, ByteArray>>>(emptyMap())
    /** Per-channel `epoch -> content-key seed` map (Phase 4b
     *  G4-3c + ADR-0031 multi-epoch retention). Persisted per
     *  account in the encrypted [logStore] from [bind] on, never
     *  sent off-device. A subscriber that missed a rotation opens
     *  the server's retained set (up to N=4 epochs) into this map
     *  on catch-up, then decrypts each post with the seed matching
     *  ITS `epoch_id`. UI shows a "waiting for seed delivery" state
     *  until at least one epoch lands here. */
    public val subscribedSeedsByChannel: StateFlow<Map<String, Map<UInt, ByteArray>>> =
        _subscribedSeedsByChannel.asStateFlow()

    /** The account the list / seeds / rotation state are bound to, or
     *  `null` before [bind]. Mutations before bind are in-memory only. */
    @Volatile
    private var boundAccountIdB64: String? = null

    /** The bound account id (base64), or `null`. */
    public val boundAccount: String?
        get() = boundAccountIdB64

    /**
     * Bind this state to an account: load its persisted channel list and
     * subscriber seeds (replacing the in-memory ones), bind the shared
     * [publisher] identity (restoring its provider snapshot), then re-open
     * the publisher keys of every listed channel this device has a persisted
     * signing seed for — so the creator can post again after a relaunch.
     * Idempotent for the same account (the store is not re-read; only
     * channels still lacking keys are re-opened). A different account
     * replaces everything account-scoped. Runs off the main thread.
     */
    public suspend fun bind(accountIdB64: String, accountIdentitySeed: ByteArray) {
        withContext(Dispatchers.Default) {
            if (boundAccountIdB64 != accountIdB64) {
                val store = logStore
                val channels = store?.loadChannels(accountIdB64) ?: emptyList()
                val seeds = store?.loadSubscribedSeeds(accountIdB64) ?: emptyMap()
                // The per-channel key material is the account's too: a second
                // account on this phone must neither reuse the first account's
                // subscriber keypair (the publisher would see one key for two
                // accounts) nor sign under its publisher seed.
                val firstBind = boundAccountIdB64 == null
                val storedSubscriberKeys = loadSubscriberKeys(accountIdB64)
                val storedSigningSeeds = loadPublisherSigningSeeds(accountIdB64)
                val storedCounters = loadPublisherCounters(accountIdB64)
                _postsByChannel.value = emptyMap()
                _epochByChannel.value = emptyMap()
                _keysByChannel.value = emptyMap()
                _channels.value = channels
                _subscribedSeedsByChannel.value = seeds
                if (firstBind) {
                    // A mutation that ran before the first bind stays, under
                    // what the store holds; a counter keeps the higher floor.
                    _subscriberKeysByChannel.value =
                        _subscriberKeysByChannel.value + storedSubscriberKeys
                    publisherSigningSeedByChannel =
                        publisherSigningSeedByChannel + storedSigningSeeds
                    _counterByChannel.value = mergeCounterFloors(_counterByChannel.value, storedCounters)
                } else {
                    _subscriberKeysByChannel.value = storedSubscriberKeys
                    publisherSigningSeedByChannel = storedSigningSeeds
                    _counterByChannel.value = storedCounters
                }
                boundAccountIdB64 = accountIdB64
                // ADR-0060 - persist at once any key [loadSubscriberKeys]
                // regenerated, so the new key survives a restart before its
                // first subscribe; and anything a first bind merged in.
                if (_subscriberKeysByChannel.value.isNotEmpty()) persistSubscriberKeys()
                if (firstBind) {
                    if (publisherSigningSeedByChannel.isNotEmpty()) persistPublisherSigningSeeds()
                    if (_counterByChannel.value.isNotEmpty()) persistPublisherCounters()
                }
            }
            publisher?.bind(accountIdB64, accountIdentitySeed)
            reopenPublisherKeys()
        }
    }

    /** Re-open the publisher keys ([channelsToReopen]) on the bound
     *  [publisher]. A channel whose open fails is skipped and logged; the
     *  owner sees it as a channel without a composer until the next bind. */
    private suspend fun reopenPublisherKeys() {
        val pub = publisher ?: return
        val targets = channelsToReopen(
            channels = _channels.value,
            signingSeedByChannel = publisherSigningSeedByChannel,
            channelsWithKeys = _keysByChannel.value.keys,
        )
        for (channel in targets) {
            try {
                val keys = pub.withIdentity { identity ->
                    ChannelsComposer.freshPublisherKeys(
                        identity = identity,
                        channelIdHex = channel.id,
                        existingSigningSeed = persistedPublisherSigningSeed(channel.id),
                    )
                }
                setKeys(channel.id, keys)
            } catch (e: Throwable) {
                Log.w(TAG, "publisher keys not re-opened for a channel", e)
            }
        }
    }

    /** Drop the in-memory account-scoped state and close the publisher
     *  identity; everything persisted stays for the next sign-in. Logout. */
    public fun unbind() {
        publisher?.unbind()
        boundAccountIdB64 = null
        _channels.value = emptyList()
        _postsByChannel.value = emptyMap()
        _epochByChannel.value = emptyMap()
        _keysByChannel.value = emptyMap()
        _subscribedSeedsByChannel.value = emptyMap()
        _subscriberKeysByChannel.value = emptyMap()
        publisherSigningSeedByChannel = emptyMap()
        _counterByChannel.value = emptyMap()
    }

    /** The persisted rotation heartbeat state for [channelId], or `null`
     *  when none (or unbound). */
    public fun rotationState(channelId: String): StoredRotationState? {
        val acct = boundAccountIdB64 ?: return null
        return logStore?.loadRotationState(acct, channelId)
    }

    /** Persist the rotation heartbeat state for [channelId]. No-op when
     *  unbound. */
    public fun saveRotationState(channelId: String, state: StoredRotationState) {
        val acct = boundAccountIdB64 ?: return
        logStore?.saveRotationState(acct, channelId, state)
    }

    /** Per-channel persisted publisher **signing** seed (G-29),
     *  restored from [publisherPrefs] for the account on [bind]. The
     *  publisher-open path ([ChannelsComposer.freshPublisherKeys])
     *  reuses the seed here so this device continues signing the
     *  channel under the same Ed25519 key across a relaunch.
     *  In-memory mirror of the encrypted store; never exposed as
     *  a flow (it drives the open path, not the UI). */
    private var publisherSigningSeedByChannel: Map<String, ByteArray> = emptyMap()

    public fun addChannel(handle: ChannelHandle) {
        _channels.update { list ->
            // Id-idempotent: re-adding refreshes the local
            // metadata rather than appending a duplicate.
            val without = list.filter { it.id != handle.id }
            listOf(handle) + without
        }
        persistChannels()
    }

    public fun removeChannel(channelId: String) {
        _channels.update { it.filter { c -> c.id != channelId } }
        _postsByChannel.update { it - channelId }
        _epochByChannel.update { it - channelId }
        _keysByChannel.update { it - channelId }
        _subscribedSeedsByChannel.update { it - channelId }
        persistChannels()
        persistSubscribedSeeds()
        boundAccountIdB64?.let { acct -> logStore?.deleteRotationState(acct, channelId) }
        // Deliberately KEEP this channel's publish counter (in-memory + at
        // rest): un-publishing does NOT destroy the MLS PublisherGroup content
        // seed, so a re-add of the same channel_id re-derives the same content
        // seed + epoch. The counter must stay a forever-monotone floor so a
        // re-add can never rewind it to 0 and re-collide the AEAD nonce (robust
        // even though the signing seed below IS dropped; matches the web client).
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
        persistChannels()
    }

    /**
     * ADR-0051 §4 — file [meta] as the channel's shared record when it is newer
     * than what is held (strictly greater stamp; the first record wins). The
     * reader's fold and the publisher's own copy both land here. Returns the
     * record now held.
     */
    public fun adoptSharedMeta(channelId: String, meta: SharedChannelMeta): SharedChannelMeta? {
        var kept: SharedChannelMeta? = null
        var changed = false
        _channels.update { list ->
            changed = false
            list.map {
                if (it.id == channelId) {
                    val held = it.sharedMeta()
                    val next = newerChannelMeta(held, meta, ::channelMetaNewer)
                    kept = next
                    if (next == held) {
                        it
                    } else {
                        changed = true
                        it.copy(
                            sharedName = next.name,
                            sharedDescription = next.description,
                            sharedUpdatedAtMs = next.updatedAtMs,
                            sharedTopic = next.topic,
                            sharedLanguage = next.language,
                            sharedListed = next.listed,
                        )
                    }
                } else {
                    it
                }
            }
        }
        // The reader re-runs classification on every refresh; a record already
        // held must not cost a prefs write each time.
        if (changed) persistChannels()
        return kept
    }

    public fun setPosts(channelId: String, posts: List<PostView>) {
        _postsByChannel.update { it + (channelId to posts) }
    }

    public fun setEpoch(channelId: String, epoch: UInt) {
        _epochByChannel.update { it + (channelId to epoch) }
    }

    /** Install publisher keys for a channel. Seeds the publish
     *  counter to 0 **only** when this channel has no persisted
     *  counter yet — an existing (relaunch-restored) counter is
     *  preserved so it stays forever-monotone. Resetting it to 0
     *  here would repeat a `(key, nonce)` pair on the stable
     *  content seed + epoch (catastrophic AEAD nonce reuse), so
     *  this mirrors the load-else-generate posture of the
     *  publisher signing seed just below. Persists the
     *  `signingKeySeed` half to the Keystore-backed store (G-29)
     *  so a relaunch reuses it via
     *  [persistedPublisherSigningSeed]. */
    public fun setKeys(channelId: String, keys: ChannelPublisherKeys) {
        _keysByChannel.update { it + (channelId to keys) }
        _counterByChannel.update { map ->
            if (map.containsKey(channelId)) map else map + (channelId to 0u)
        }
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
     *  counter and persist the advanced floor BEFORE returning.
     *  Returns the value the NEXT post should carry. Persisting
     *  before hand-off means a crash right after a post can only
     *  ever SKIP a counter, never hand the same value out twice —
     *  the safe direction for AEAD nonce uniqueness. */
    public fun nextCounter(channelId: String): ULong {
        var value: ULong = 0u
        _counterByChannel.update { map ->
            value = map[channelId] ?: 0u
            map + (channelId to (value + 1u))
        }
        persistPublisherCounters()
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
        persistSubscribedSeeds()
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
        persistSubscribedSeeds()
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
        persistPublisherCounters()
        persistChannels()
        persistSubscribedSeeds()
    }

    // -----------------------------------------------------------------
    // Channel-log persistence (list + subscriber seeds), per account
    // -----------------------------------------------------------------

    /** Write the channel list through to the bound account's log. A
     *  mutation before [bind] stays in memory (there is no account to key
     *  it by) and is replaced by the persisted list when bind runs. */
    private fun persistChannels() {
        val acct = boundAccountIdB64 ?: return
        val store = logStore ?: return
        try {
            store.saveChannels(acct, _channels.value)
        } catch (e: Throwable) {
            Log.w(TAG, "channel list not persisted", e)
        }
    }

    private fun persistSubscribedSeeds() {
        val acct = boundAccountIdB64 ?: return
        val store = logStore ?: return
        try {
            store.saveSubscribedSeeds(acct, _subscribedSeedsByChannel.value)
        } catch (e: Throwable) {
            Log.w(TAG, "subscriber seeds not persisted", e)
        }
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
        val acct = boundAccountIdB64 ?: return
        val arr = JSONArray()
        for ((channelId, keys) in _subscriberKeysByChannel.value) {
            val row = JSONObject()
            row.put("channelId", channelId)
            row.put("secretHex", ChannelsComposer.bytesToHex(keys.secret))
            row.put("publicHex", ChannelsComposer.bytesToHex(keys.publicKey))
            // ADR-0060 section 3 - the key scheme: Ed25519 since the
            // member-card amendment. A row without it holds an older key.
            row.put("scheme", SUBSCRIBER_KEY_SCHEME_ED25519)
            arr.put(row)
        }
        sink.edit().putString(accountKey(SUBSCRIBER_KEYS_PREF_KEY, acct), arr.toString()).apply()
    }

    private fun loadSubscriberKeys(accountIdB64: String): Map<String, ChannelSubscriberKeys> {
        val raw = prefs?.getString(accountKey(SUBSCRIBER_KEYS_PREF_KEY, accountIdB64), null) ?: return emptyMap()
        val arr = runCatching { JSONArray(raw) }.getOrNull() ?: return emptyMap()
        val out = LinkedHashMap<String, ChannelSubscriberKeys>(arr.length())
        for (i in 0 until arr.length()) {
            val row = arr.optJSONObject(i) ?: continue
            val channelId = row.optString("channelId").takeIf { it.isNotEmpty() } ?: continue
            if (row.optString("scheme") != SUBSCRIBER_KEY_SCHEME_ED25519) {
                // ADR-0060 section 3 - a stored X25519 subscriber key cannot
                // sign a member card. A fresh Ed25519 key replaces it; the
                // reader's idempotent re-subscribe registers it and the
                // publisher reseals the seed to it at the next fanout. No
                // migration of the old key: there is no deployment to carry.
                out[channelId] = ChannelsComposer.freshSubscriberKeys()
                continue
            }
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
        val acct = boundAccountIdB64 ?: return
        val arr = JSONArray()
        for ((channelId, seed) in publisherSigningSeedByChannel) {
            val row = JSONObject()
            row.put("channelId", channelId)
            row.put("signingSeedHex", ChannelsComposer.bytesToHex(seed))
            arr.put(row)
        }
        sink.edit().putString(accountKey(PUBLISHER_SIGNING_SEEDS_PREF_KEY, acct), arr.toString()).apply()
    }

    private fun loadPublisherSigningSeeds(accountIdB64: String): Map<String, ByteArray> {
        val raw = publisherPrefs?.getString(accountKey(PUBLISHER_SIGNING_SEEDS_PREF_KEY, accountIdB64), null)
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

    // -----------------------------------------------------------------
    // Publisher counter persistence (AEAD nonce-reuse defence)
    // -----------------------------------------------------------------

    /** JSON array of `{channelId, counter}` rows, the counter as a
     *  decimal string (JSON has no unsigned-64 type). Persisted into
     *  [publisherPrefs] (the same Keystore-backed store as the signing
     *  seeds) whenever the counter map changes, so the forever-monotone
     *  floor survives a relaunch. */
    private fun persistPublisherCounters() {
        val sink = publisherPrefs ?: return
        val acct = boundAccountIdB64 ?: return
        val arr = JSONArray()
        for ((channelId, counter) in _counterByChannel.value) {
            val row = JSONObject()
            row.put("channelId", channelId)
            row.put("counter", counter.toString())
            arr.put(row)
        }
        sink.edit().putString(accountKey(PUBLISHER_COUNTERS_PREF_KEY, acct), arr.toString()).apply()
    }

    private fun loadPublisherCounters(accountIdB64: String): Map<String, ULong> {
        val raw = publisherPrefs?.getString(accountKey(PUBLISHER_COUNTERS_PREF_KEY, accountIdB64), null)
            ?: return emptyMap()
        val arr = runCatching { JSONArray(raw) }.getOrNull() ?: return emptyMap()
        val out = LinkedHashMap<String, ULong>(arr.length())
        for (i in 0 until arr.length()) {
            val row = arr.optJSONObject(i) ?: continue
            val channelId = row.optString("channelId").takeIf { it.isNotEmpty() } ?: continue
            val counter = row.optString("counter").toULongOrNull() ?: continue
            out[channelId] = counter
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
    /** The public `@handle` this device registered for the channel, or ""
     *  when none was claimed (local display only; the wire form is the id). */
    public val handle: String = "",
    /** ADR-0051 §4 — the name from the newest metadata record this device
     *  read (a subscriber) or posted (the publisher); "" when none. */
    public val sharedName: String = "",
    /** ADR-0051 §4 — the description from that same record. */
    public val sharedDescription: String = "",
    /** ADR-0051 §4 — the record's publisher stamp; `null` when no record has
     *  landed. A re-issue carries this value verbatim. */
    public val sharedUpdatedAtMs: ULong? = null,
    /** ADR-0059 metadata v2 — the topic id of that record (0 = Other). */
    public val sharedTopic: Int = 0,
    /** ADR-0059 metadata v2 — its ISO 639 language, "" when not set. */
    public val sharedLanguage: String = "",
    /** ADR-0059 metadata v2 — the owner listed the channel in Discover; what
     *  lets a member share it in a group (section 6). */
    public val sharedListed: Boolean = false,
)
