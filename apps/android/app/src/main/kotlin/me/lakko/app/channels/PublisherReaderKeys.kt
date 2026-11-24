// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.content.SharedPreferences

/**
 * Every publishing account's reader key, known to the channel's other
 * publishers (`docs/ux/group-and-channel-privacy.md` section 15.1; live walk
 * 2026-11-24, "Night walk": web owner, iOS publisher). Mirror of web
 * `channels/publisher-reader-keys.ts`.
 *
 * The device that holds the PublisherGroup registers its account's reader
 * key at the channels service as a subscriber row ([ChannelAccountReaderKey]).
 * The owner saw a co-publisher's row as "From an invitation" with `Remove`
 * and counted it, and the publisher saw the owner's the same way. Each
 * publishing device now announces its account's reader key on the
 * PublisherGroup (`lakko-contacts::publisher_reader_key`) once registered and
 * again when the group grows; every publishing device keeps the newest per
 * current publisher account (sender resolved at receipt, C-68) and leaves
 * those keys out of the subscriber list and count.
 */
public object PublisherReaderKeys {
    /** The receiver's rule, pure: kept only for this channel, from a sender
     *  that is a current publisher account. (account base64, reader hex). */
    public fun verdict(
        channelId: ByteArray,
        envelopeChannelId: ByteArray,
        readerKey: ByteArray,
        sender: ByteArray?,
        publisherAccounts: List<ByteArray>,
    ): Pair<String, String>? {
        if (!envelopeChannelId.contentEquals(channelId) || sender == null || readerKey.size != 32) return null
        if (publisherAccounts.none { it.contentEquals(sender) }) return null
        return b64(sender) to ChannelsComposer.bytesToHex(readerKey).lowercase()
    }

    /** The stored keys whose account still publishes, pure. */
    public fun pruned(stored: Map<String, String>, publisherAccounts: List<ByteArray>): Map<String, String> {
        val accounts = publisherAccounts.map(::b64).toSet()
        return stored.filterKeys { it in accounts }
    }

    /** When to announce, pure: once registered, again when the key or the
     *  number of PublisherGroup leaves changed since the last announcement. */
    public fun sendDue(registeredHex: String?, leaves: Int, lastSent: String?): String? {
        if (registeredHex.isNullOrEmpty()) return null
        val sig = "${registeredHex.lowercase()}|$leaves"
        return if (sig == lastSent) null else sig
    }

    /** The map's wire form in the preferences, pure. */
    internal fun encode(map: Map<String, String>): String =
        map.entries.sortedBy { it.key }.joinToString(";") { "${it.key}=${it.value}" }

    /** The inverse of [encode]; malformed entries are dropped. */
    internal fun decode(raw: String?): Map<String, String> {
        if (raw.isNullOrEmpty()) return emptyMap()
        return raw.split(';').mapNotNull { entry ->
            // The key is standard base64 and ends in `=`; the hex value has
            // none, so the separator is the last `=`.
            val at = entry.lastIndexOf('=')
            if (at <= 0) return@mapNotNull null
            val key = entry.substring(0, at)
            val value = entry.substring(at + 1).lowercase()
            if (key.length != 44 || !Regex("^[0-9a-f]{64}$").matches(value)) null else key to value
        }.toMap()
    }

    private fun key(account: String, channelId: String): String =
        "publisher-reader-keys|$account|${channelId.lowercase()}"

    /** The other publishers' reader keys of [channelId] this device holds. */
    public fun stored(context: Context, account: String, channelId: String): Map<String, String> = try {
        stored(ChannelKeyStore.encryptedPrefs(context), account, channelId)
    } catch (_: Throwable) {
        emptyMap()
    }

    internal fun stored(prefs: SharedPreferences, account: String, channelId: String): Map<String, String> =
        decode(prefs.getString(key(account, channelId), null))

    internal fun store(prefs: SharedPreferences, account: String, channelId: String, map: Map<String, String>) {
        val edit = prefs.edit()
        if (map.isEmpty()) edit.remove(key(account, channelId)) else edit.putString(key(account, channelId), encode(map))
        edit.apply()
    }

    /** Keep the reader keys a journal poll read from current publishers and
     *  drop those of accounts that no longer publish. */
    public fun receive(
        context: Context,
        account: String,
        channelId: String,
        applications: List<ByteArray>,
        senders: List<ByteArray?>,
        publisherAccounts: List<ByteArray>,
    ) {
        if (publisherAccounts.isEmpty()) return
        val prefs = try {
            ChannelKeyStore.encryptedPrefs(context)
        } catch (_: Throwable) {
            return
        }
        val before = stored(prefs, account, channelId)
        val next = pruned(before, publisherAccounts).toMutableMap()
        val channelBytes = ChannelsComposer.channelIdFromHex(channelId)
        val envelopeLen = uniffi.lakko_ffi_uniffi.publisherReaderKeyEnvelopeLen().toInt()
        applications.forEachIndexed { i, plaintext ->
            if (plaintext.size != envelopeLen) return@forEachIndexed
            val decoded = try {
                uniffi.lakko_ffi_uniffi.decodePublisherReaderKeyEnvelope(plaintext)
            } catch (_: Throwable) {
                return@forEachIndexed
            }
            verdict(channelBytes, decoded.channelId, decoded.readerKey, senders.getOrNull(i), publisherAccounts)
                ?.let { (acct, hex) -> next[acct] = hex }
        }
        if (next != before) store(prefs, account, channelId, next)
    }

    private fun b64(bytes: ByteArray): String = java.util.Base64.getEncoder().encodeToString(bytes)
}
