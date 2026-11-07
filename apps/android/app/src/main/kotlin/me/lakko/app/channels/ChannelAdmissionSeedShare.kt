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

    /** A request's carried account id, kept by the joiner key it came with. */
    public fun rememberRequest(context: Context, account: String, channelId: String, joinerKey: ByteArray, accountId: ByteArray?) {
        if (accountId == null || accountId.size != 16) return
        prefs(context).edit().putString("req|$account|$channelId|${hex(joinerKey)}", hex(accountId)).apply()
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

    /** Whether any publisher of [channelId] still lacks an admission seed
     *  version or the read key (no network, no decryption). */
    public fun anyOwed(context: Context, account: String, channelId: String): Boolean {
        val versions = ChannelAdmissionKeys.all(context, account, channelId).let { all ->
            try {
                all.map { it.first.toString() }
            } finally {
                all.forEach { it.second.fill(0) }
            }
        }
        return publishers(context, account, channelId).any { (keyHex, _) ->
            val already = sent(context, account, channelId, keyHex)
            READ_KEY_MARK !in already || versions.any { it !in already }
        }
    }

    /** The owner's pass: seal what each publisher lacks — every admission
     *  seed version and the channel's read key. Best effort. */
    public suspend fun sendOwed(
        context: Context,
        account: String,
        channelId: String,
        identity: Identity,
        group: PublisherGroup,
        ds: me.lakko.app.ds.DsClient,
        /** Snapshot the provider, throwing when it was not written. Called
         *  after each message is sealed and before it is published, so a
         *  crash between the two never reuses a ratchet generation (G-34
         *  class); a failure publishes nothing. */
        persistNow: () -> Unit,
    ) {
        val seeds = ChannelAdmissionKeys.all(context, account, channelId)
        try {
            for ((keyHex, accountIdHex) in publishers(context, account, channelId)) {
                val already = sent(context, account, channelId, keyHex)
                val owed = seeds.filter { it.first.toString() !in already }
                val readKeyOwed = READ_KEY_MARK !in already
                if (owed.isEmpty() && !readKeyOwed) continue
                val accountId = ByteArray(16) { i -> accountIdHex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
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
                    val message = group.createApplicationMessage(identity, envelope)
                    persistNow()
                    ds.publishGroup(group.groupId(), message, signer = CoPublisher.signerOf(group, identity))
                    marks = marks + version.toString()
                    prefs(context).edit().putStringSet("sent|$account|$channelId|$keyHex", marks).apply()
                }
                if (readKeyOwed) {
                    // ADR-0087 section 15 (phase B) — the channel's read key,
                    // so a publisher mints public links and files public seeds.
                    val readKey = ChannelDiscovery.readKey(identity, channelId)
                    try {
                        val envelope = uniffi.lakko_ffi_uniffi.composeChannelReadKeyEnvelope(
                            group.channelId(),
                            accountKey,
                            x,
                            readKey,
                        )
                        val message = group.createApplicationMessage(identity, envelope)
                        persistNow()
                        ds.publishGroup(group.groupId(), message, signer = CoPublisher.signerOf(group, identity))
                    } finally {
                        readKey.fill(0)
                    }
                    marks = marks + READ_KEY_MARK
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

    /** Forget device / Delete account. */
    public fun forgetAccount(context: Context, account: String) {
        val p = prefs(context)
        val editor = p.edit()
        p.all.keys.filter { it.contains("|$account|") }.forEach(editor::remove)
        editor.apply()
    }
}
