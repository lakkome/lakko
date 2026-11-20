// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.util.Base64
import android.util.Log
import uniffi.lakko_ffi_uniffi.Identity
import uniffi.lakko_ffi_uniffi.PublisherGroup

/**
 * ADR-0087 section 7 — the owner hands each publisher every admission-seed
 * version it lacks, sealed to that person's ACCOUNT X25519 key, as an
 * application message on the PublisherGroup journal, so any publisher's
 * device can check tickets and let people in. The key comes from the
 * person's identity bundle in the directory, fetched by the account id their
 * request carried in its profile, and used only when the bundle's signature
 * key IS the publisher's account key. Versions sent are recorded per
 * (channel, publisher); a failure is retried by the next pass.
 */
public object ChannelAdmissionSeedShare {
    private const val TAG = "AdmissionSeedShare"
    private const val PREFS = "lakko.channels.copublishers"
    private const val READ_KEY_MARK = "read-key"

    private fun prefs(context: Context) = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun hex(b: ByteArray): String = b.joinToString("") { "%02x".format(it) }

    /**
     * A request's carried account id, kept by the joiner key it came with. A
     * request carries the id only in a profile, and a person with no display
     * name has none (ADR-0048): such a request is kept too, with the id this
     * account's contact row names, or `""` until [sendOwed] finds one.
     */
    public fun rememberRequest(context: Context, account: String, channelId: String, joinerKey: ByteArray, accountId: ByteArray?) {
        val key = "req|$account|$channelId|${hex(joinerKey)}"
        val carried = if (accountId != null && accountId.size == 16) hex(accountId) else ""
        val resolved = resolvePublisherAccountIdHex(carried, joinerKey) { contactsOf(context, account) }
        // A later sighting of the same request never forgets an id held.
        if (resolved.isEmpty() && !prefs(context).getString(key, null).isNullOrEmpty()) return
        prefs(context).edit().putString(key, resolved).apply()
    }

    private fun contactsOf(context: Context, account: String): List<me.lakko.app.contacts.AcceptedContact> =
        try {
            me.lakko.app.contacts.AcceptedContactsStore.load(context, account)
        } catch (_: Throwable) {
            emptyList()
        }

    /**
     * Fill in the account id of every publisher recorded without one, from
     * this account's contact store, and keep it. Returns the publishers the
     * owner can reach now: joiner key (hex) to account id (hex).
     */
    internal fun fillPublisherAccountIds(context: Context, account: String, channelId: String): Map<String, String> {
        val out = HashMap<String, String>()
        var contacts: List<me.lakko.app.contacts.AcceptedContact>? = null
        for ((keyHex, accountIdHex) in publishers(context, account, channelId)) {
            if (isAccountIdHex(accountIdHex)) {
                out[keyHex] = accountIdHex
                continue
            }
            val key = unhex(keyHex) ?: continue
            val resolved = resolvePublisherAccountIdHex(accountIdHex, key) {
                contacts ?: contactsOf(context, account).also { contacts = it }
            }
            if (resolved.isEmpty()) continue
            prefs(context).edit().putString("pub|$account|$channelId|$keyHex", resolved).apply()
            out[keyHex] = resolved
        }
        return out
    }

    /** After an approve: the joiner is a publisher to send seeds to. */
    public fun notePublisher(context: Context, account: String, channelId: String, joinerKey: ByteArray) {
        val id = prefs(context).getString("req|$account|$channelId|${hex(joinerKey)}", null) ?: return
        prefs(context).edit().putString("pub|$account|$channelId|${hex(joinerKey)}", id).apply()
    }

    private fun publishers(context: Context, account: String, channelId: String): List<Pair<String, String>> {
        val prefix = "pub|$account|$channelId|"
        return prefs(context).all.mapNotNull { (k, v) ->
            if (k.startsWith(prefix)) k.removePrefix(prefix) to (v as? String ?: return@mapNotNull null) else null
        }
    }

    private fun sent(context: Context, account: String, channelId: String, keyHex: String): Set<String> =
        prefs(context).getStringSet("sent|$account|$channelId|$keyHex", emptySet()).orEmpty()

    /**
     * The key to seal to for the publisher whose request carried
     * [requestKey], given its identity [bundle] as the directory serves it
     * now: the bundle's current key, but only when it is proven to be the
     * same person as the request key — the same key, the bundle's two-way
     * conversation link naming the request key (ADR-0074 section 3.7), or a
     * chain of proven steps in this device's predecessor store. Anything
     * else is `null`: an unproven key change is another person.
     */
    internal fun provenCurrentKey(
        context: Context,
        account: String,
        requestKey: ByteArray,
        bundle: me.lakko.app.discovery.IdentityFetchResponse,
    ): ByteArray? {
        val current = try {
            Base64.decode(bundle.signature_pubkey, Base64.DEFAULT)
        } catch (_: IllegalArgumentException) {
            return null
        }
        if (current.size != 32) return null
        if (current.contentEquals(requestKey)) return current
        val link = bundle.conversation_link?.let {
            try {
                Base64.decode(it, Base64.DEFAULT)
            } catch (_: IllegalArgumentException) {
                null
            }
        }
        if (link != null) {
            val conversationKey = try {
                uniffi.lakko_ffi_uniffi.verifyConversationLink(link, current)
            } catch (_: Throwable) {
                null
            }
            if (conversationKey != null && conversationKey.contentEquals(requestKey)) return current
        }
        val links = try {
            me.lakko.app.contacts.SharedPrefsProvenLinkStore(context).load(account)
        } catch (_: Throwable) {
            emptyList()
        }
        val requestB64 = Base64.encodeToString(requestKey, Base64.NO_WRAP)
        val currentB64 = Base64.encodeToString(current, Base64.NO_WRAP)
        return if (me.lakko.app.contacts.samePerson(requestB64, currentB64, links)) current else null
    }

    /** Whether any publisher of [channelId] this device can reach still
     *  lacks an admission seed version, the read key, or one of the
     *  channel's earlier seeds ([epochs], the owner's epoch map) (no network,
     *  no decryption). */
    public fun anyOwed(
        context: Context,
        account: String,
        channelId: String,
        epochs: Set<UInt> = emptySet(),
        maxSeeds: Int = EPOCH_SEEDS_MAX,
    ): Boolean {
        val versions = ChannelAdmissionKeys.all(context, account, channelId).let { all ->
            try {
                all.map { (v, seed) -> admissionSeedMark(v, seed) }
            } finally {
                all.forEach { it.second.fill(0) }
            }
        }
        // A publisher no account id names yet is not owed anything this
        // device can send (its contact row fills the id when there is one).
        return fillPublisherAccountIds(context, account, channelId).keys.any { keyHex ->
            val already = sent(context, account, channelId, keyHex)
            READ_KEY_MARK !in already || versions.any { it !in already } ||
                epochSeedEpochsToSend(epochs, already, maxSeeds).isNotEmpty()
        }
    }

    /** The owner's pass: seal what each publisher lacks — every admission
     *  seed version and the channel's read key. Best effort. */
    public suspend fun sendOwed(
        context: Context,
        account: String,
        channelId: String,
        ids: ChannelIdentity,
        group: PublisherGroup,
        ds: me.lakko.app.ds.DsClient,
        /** ADR-0087 section 15 (2026-11-08) — the owner's epoch map, every
         *  channel seed this device holds by epoch: the newest
         *  [EPOCH_SEEDS_MAX] a publisher lacks go to it in one envelope. */
        epochSeeds: Map<UInt, ByteArray> = emptyMap(),
    ) {
        val seeds = ChannelAdmissionKeys.all(context, account, channelId)
        val maxSeeds = try {
            uniffi.lakko_ffi_uniffi.channelEpochSeedsMax().toInt()
        } catch (_: Throwable) {
            EPOCH_SEEDS_MAX
        }
        try {
            for ((keyHex, accountIdHex) in fillPublisherAccountIds(context, account, channelId)) {
                val already = sent(context, account, channelId, keyHex)
                // ADR-0090 — EVERY seed of a version, not one per version.
                val owed = seeds.filter { (v, seed) -> admissionSeedMark(v, seed) !in already }
                val readKeyOwed = READ_KEY_MARK !in already
                val epochsOwed = epochSeedEpochsToSend(epochSeeds.keys, already, maxSeeds)
                if (owed.isEmpty() && !readKeyOwed && epochsOwed.isEmpty()) continue
                val accountId = unhex(accountIdHex)?.takeIf { it.size == 16 } ?: continue
                val bundle = try {
                    me.lakko.app.discovery.DiscoveryClient().use { d ->
                        d.setSessionToken(me.lakko.app.auth.SessionTokens.provider?.current())
                        d.identityFetch(accountId)
                    }
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (_: Throwable) {
                    null
                } ?: continue
                val requestKey = ByteArray(32) { i -> keyHex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
                val accountKey = provenCurrentKey(context, account, requestKey, bundle) ?: run {
                    Log.i(TAG, "a publisher's key changed with no proof; nothing sealed to it")
                    null
                } ?: continue
                val x = Base64.decode(bundle.x25519_pubkey, Base64.DEFAULT).takeIf { it.size == 32 } ?: continue
                var marks = already
                for ((version, seed) in owed) {
                    val envelope = uniffi.lakko_ffi_uniffi.composeAdmissionSeedEnvelope(
                        group.channelId(),
                        accountKey,
                        x,
                        version,
                        seed,
                    )
                    CoPublisher.publishApplication(ds, ids, group, envelope)
                    marks = marks + admissionSeedMark(version, seed)
                    prefs(context).edit().putStringSet("sent|$account|$channelId|$keyHex", marks).apply()
                }
                if (readKeyOwed) {
                    // ADR-0087 section 15 (phase B) — the channel's read key,
                    // so a publisher mints public links and files public seeds.
                    val readKey = ChannelDiscovery.readKey(ids.account, channelId)
                    try {
                        val envelope = uniffi.lakko_ffi_uniffi.composeChannelReadKeyEnvelope(
                            group.channelId(),
                            accountKey,
                            x,
                            readKey,
                        )
                        CoPublisher.publishApplication(ds, ids, group, envelope)
                    } finally {
                        readKey.fill(0)
                    }
                    marks = marks + READ_KEY_MARK
                    prefs(context).edit().putStringSet("sent|$account|$channelId|$keyHex", marks).apply()
                }
                if (epochsOwed.isNotEmpty()) {
                    // ADR-0087 section 15 (2026-11-08) — the channel's earlier
                    // seeds, so a new publisher reads what came before it.
                    val envelope = uniffi.lakko_ffi_uniffi.composeChannelEpochSeedsEnvelope(
                        group.channelId(),
                        accountKey,
                        x,
                        epochsOwed.map { e -> uniffi.lakko_ffi_uniffi.FfiChannelEpochSeed(e, epochSeeds.getValue(e)) },
                    )
                    CoPublisher.publishApplication(ds, ids, group, envelope)
                    marks = marks + epochsOwed.map(::epochSeedMark)
                    prefs(context).edit().putStringSet("sent|$account|$channelId|$keyHex", marks).apply()
                }
            }
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Throwable) {
            Log.i(TAG, "keys not sealed to every publisher yet")
        } finally {
            seeds.forEach { it.second.fill(0) }
        }
    }

    /**
     * ADR-0090 section 6 — a device of [accountKey]'s account was admitted:
     * that account is owed the read key, every admission seed and the epoch
     * seeds again (its new device holds none; a device that does drops the
     * duplicates). For this account's own key the publisher record is made
     * here, under this account's id. Best effort.
     */
    public fun owedAgain(account: String, channelId: String, accountKey: ByteArray, ownAccountKey: ByteArray) {
        val context = SubscriberAdmission.appContext ?: return
        val keyHex = hex(accountKey)
        val editor = prefs(context).edit().remove("sent|$account|$channelId|$keyHex")
        if (accountKey.contentEquals(ownAccountKey)) {
            val accountId = try {
                java.util.Base64.getDecoder().decode(account)
            } catch (_: IllegalArgumentException) {
                null
            }
            if (accountId != null && accountId.size == 16) {
                editor.putString("pub|$account|$channelId|$keyHex", hex(accountId))
            }
        }
        editor.apply()
    }

    /** A removed publisher gets nothing more: its record goes. */
    public fun forgetPublisher(context: Context, account: String, channelId: String, accountKey: ByteArray) {
        val keyHex = hex(accountKey)
        prefs(context).edit()
            .remove("pub|$account|$channelId|$keyHex")
            .remove("sent|$account|$channelId|$keyHex")
            .apply()
    }

    /** A publisher's side: the channel's read key the owner sealed to this
     *  account; kept only when it opens (addressed to this account). */
    public fun receiveReadKey(context: Context, account: String, channelId: String, identity: Identity, plaintext: ByteArray) {
        val readKey = try {
            uniffi.lakko_ffi_uniffi.openChannelReadKeyEnvelope(plaintext, identity)
        } catch (_: Throwable) {
            return
        }
        try {
            ChannelReadKeys.put(context, account, channelId, readKey)
        } finally {
            readKey.fill(0)
        }
    }

    /** A publisher's side: an application message the owner sent it, kept
     *  when it is an admission seed addressed to this account. */
    public fun receive(context: Context, account: String, channelId: String, identity: Identity, plaintext: ByteArray) {
        val env = try {
            uniffi.lakko_ffi_uniffi.decodeAdmissionSeedEnvelope(plaintext)
        } catch (_: Throwable) {
            return
        }
        val seed = try {
            uniffi.lakko_ffi_uniffi.openAdmissionSeedEnvelope(plaintext, identity)
        } catch (_: Throwable) {
            return
        }
        try {
            ChannelAdmissionKeys.put(context, account, channelId, env.admissionVersion, seed)
        } finally {
            seed.fill(0)
        }
    }

    /**
     * A publisher's side (ADR-0087 section 15, 2026-11-08): the channel's
     * earlier seeds the owner sealed to this account. The caller has checked
     * that it came on [channelId]'s PublisherGroup journal from the owner
     * (resolved at receipt) and that this device publishes the channel.
     * Kept only when it names this channel and this account and opens; a
     * seed is added only for an epoch with none held, never overwriting.
     * Returns how many seeds were added.
     */
    public fun receiveEpochSeeds(
        state: ChannelsUiState,
        channelId: String,
        identity: Identity,
        plaintext: ByteArray,
    ): Int {
        val env = try {
            uniffi.lakko_ffi_uniffi.decodeChannelEpochSeedsEnvelope(plaintext)
        } catch (_: Throwable) {
            return 0
        }
        if (!env.channelId.contentEquals(ChannelsComposer.channelIdFromHex(channelId))) return 0
        if (!env.memberAccountKey.contentEquals(identity.signaturePublicKey())) return 0
        val opened = try {
            uniffi.lakko_ffi_uniffi.openChannelEpochSeedsEnvelope(plaintext, identity)
        } catch (_: Throwable) {
            return 0
        }
        val keep = epochSeedsToKeep(opened.map { it.epoch to it.seed }, state.subscribedSeeds(channelId).keys)
        for ((epoch, seed) in keep) state.setSubscribedSeed(channelId, epoch, seed)
        opened.forEach { it.seed.fill(0) }
        return keep.size
    }

    /** Forget device / Delete account. */
    public fun forgetAccount(context: Context, account: String) {
        val p = prefs(context)
        val editor = p.edit()
        p.all.keys.filter { it.contains("|$account|") }.forEach(editor::remove)
        editor.apply()
    }
}

/** Lower-case hex of a 16-byte account id. */
internal fun isAccountIdHex(s: String): Boolean = s.length == 32 && s.all { it in '0'..'9' || it in 'a'..'f' || it in 'A'..'F' }

private fun unhex(s: String): ByteArray? {
    if (s.length % 2 != 0) return null
    return try {
        ByteArray(s.length / 2) { i -> s.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
    } catch (_: NumberFormatException) {
        null
    }
}

/**
 * ADR-0087 section 7 — the 16-byte account id (lower-case hex) the owner
 * fetches a publisher's identity bundle by: the one the request carried
 * ([recordedHex]), else the one this account's contact row for
 * [accountKey] names (the person added through "Add publisher" is a
 * contact). The id only picks the bundle; nothing is sealed unless the
 * bundle proves the same person ([ChannelAdmissionSeedShare.provenCurrentKey]).
 * `""` when neither names one. [contacts] is read only when needed.
 */
internal fun resolvePublisherAccountIdHex(
    recordedHex: String,
    accountKey: ByteArray,
    contacts: () -> List<me.lakko.app.contacts.AcceptedContact>,
): String {
    if (isAccountIdHex(recordedHex)) return recordedHex.lowercase()
    val id = contacts().firstOrNull { it.request.fromPubkey.contentEquals(accountKey) }?.request?.fromAccountId
        ?: return ""
    if (id.size != 16) return ""
    return id.joinToString("") { "%02x".format(it) }
}

/** ADR-0090 — the sent mark of one admission seed: its version and a short
 *  tag of the seed, so every seed of a version is sent. */
internal fun admissionSeedMark(version: UShort, seed: ByteArray): String =
    "$version:" + java.security.MessageDigest.getInstance("SHA-256").digest(seed).take(6).joinToString("") { "%02x".format(it) }

/** The most seeds one earlier-seeds envelope carries (`channelEpochSeedsMax`). */
internal const val EPOCH_SEEDS_MAX: Int = 64

/** The sent mark of the seed of [epoch]. */
internal fun epochSeedMark(epoch: UInt): String = "e:$epoch"

/**
 * ADR-0087 section 15 (2026-11-08) — the epochs whose seeds go to a
 * publisher now: of the owner's [held] epochs, the newest [max], less those
 * already marked sent to it ([sent]); ascending.
 */
internal fun epochSeedEpochsToSend(held: Set<UInt>, sent: Set<String>, max: Int = EPOCH_SEEDS_MAX): List<UInt> =
    held.sortedDescending().take(max).filter { epochSeedMark(it) !in sent }.sorted()

/** The opened seeds to add: only epochs with no seed held, never an overwrite
 *  (the first one filed for an epoch wins). */
internal fun epochSeedsToKeep(opened: List<Pair<UInt, ByteArray>>, held: Set<UInt>): List<Pair<UInt, ByteArray>> {
    val seen = HashSet<UInt>(held)
    return opened.filter { (epoch, _) -> seen.add(epoch) }.map { (e, s) -> e to s.copyOf() }
}
