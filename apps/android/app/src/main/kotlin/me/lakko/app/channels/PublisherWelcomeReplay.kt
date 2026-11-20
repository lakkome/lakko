// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject

/**
 * Security pass F2 (2026-11-15) - the PublisherGroup Welcomes this device has
 * JOINED, per channel, as SHA-256 hashes of the Welcome bytes. A device's
 * KeyPackage is last-resort (ADR-0050 2.6), so the Delivery Service can
 * replay an old owner-signed Welcome after raising a reset suspicion it
 * cannot be told apart from ([ChannelSuspensions]); joining it would roll the
 * device back to an old epoch. Every landing path
 * ([CoPublisher.joinReplacingHeld]) refuses a Welcome whose hash is here: an
 * old Welcome is one this device already joined, while a re-founded
 * channel's Welcome is new.
 *
 * Per account, keyed by channel, at most [MAX_PER_CHANNEL] per channel
 * (newest kept). Plain preferences (a hash of a ciphertext is no secret);
 * cleared with the channel and by the account wipe.
 */
public object JoinedPublisherWelcomes {
    /** Hashes kept per channel (chosen, not measured). */
    public const val MAX_PER_CHANNEL: Int = 16

    private const val PREFS = "lakko.channels.joined-welcomes"

    private fun prefs(context: Context) = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun key(account: String) = "joined|$account"

    /** The hash a Welcome is kept under (lower-case hex SHA-256). */
    public fun hashOf(welcome: ByteArray): String =
        java.security.MessageDigest.getInstance("SHA-256").digest(welcome).joinToString("") { "%02x".format(it) }

    private fun loadAll(context: Context, account: String): MutableMap<String, List<String>> {
        val raw = prefs(context).getString(key(account), null) ?: return mutableMapOf()
        return try {
            val o = JSONObject(raw)
            val out = mutableMapOf<String, List<String>>()
            for (channel in o.keys()) {
                val arr = o.getJSONArray(channel)
                out[channel] = (0 until arr.length()).map { arr.getString(it) }
            }
            out
        } catch (_: org.json.JSONException) {
            mutableMapOf()
        }
    }

    private fun saveAll(context: Context, account: String, all: Map<String, List<String>>): Boolean {
        val o = JSONObject()
        for ((channel, list) in all) {
            if (list.isNotEmpty()) o.put(channel, JSONArray(list))
        }
        return prefs(context).edit().putString(key(account), o.toString()).commit()
    }

    /** Whether this device already joined [welcome] for [channelId]. */
    public fun contains(context: Context, account: String, channelId: String, welcome: ByteArray): Boolean =
        hashOf(welcome) in loadAll(context, account)[channelId.lowercase()].orEmpty()

    /** Record that [welcome] was joined for [channelId]. `false` when it did
     *  not reach the disk. */
    public fun record(context: Context, account: String, channelId: String, welcome: ByteArray): Boolean {
        val all = loadAll(context, account)
        val id = channelId.lowercase()
        all[id] = joinedWelcomesAfterAdd(all[id].orEmpty(), hashOf(welcome))
        return runCatching { saveAll(context, account, all) }.getOrDefault(false)
    }

    /** The channel ended on this device. */
    public fun clear(context: Context, account: String, channelId: String) {
        val all = loadAll(context, account)
        if (all.remove(channelId.lowercase()) != null) saveAll(context, account, all)
    }

    /** The account wipe. */
    public fun forgetAccount(context: Context, account: String) {
        prefs(context).edit().remove(key(account)).commit()
    }
}

/** The joined list after [hash] was joined: kept once, oldest first, at most
 *  [JoinedPublisherWelcomes.MAX_PER_CHANNEL] (the newest). Pure. */
public fun joinedWelcomesAfterAdd(joined: List<String>, hash: String): List<String> =
    (joined.filterNot { it == hash } + hash).takeLast(JoinedPublisherWelcomes.MAX_PER_CHANNEL)

/**
 * Security pass F3 (2026-11-15) - the epoch of the last PublisherGroup this
 * device deleted for a channel (an inactive group before an entry or a
 * re-founding, or one dropped after an entry that lost). With no group held,
 * a Welcome would otherwise join with no floor at all;
 * [joinWithoutHeldGroupAllowed] refuses a joined group at an epoch not above
 * this one unless the channel is under reset suspicion (a re-founded chain
 * starts again at a low epoch). Per account, plain preferences; cleared when
 * a Welcome joins, with the channel, and by the account wipe.
 */
public object DeletedPublisherGroupEpochs {
    private const val PREFS = "lakko.channels.deleted-group-epochs"

    private fun prefs(context: Context) = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun key(account: String, channelId: String) = "deleted|$account|${channelId.lowercase()}"

    /** The deleted group's epoch for [channelId], or `null`. */
    public fun get(context: Context, account: String, channelId: String): ULong? =
        prefs(context).getString(key(account, channelId), null)?.toULongOrNull()

    /** Record the epoch of the group just deleted (the newest deletion wins). */
    public fun save(context: Context, account: String, channelId: String, epoch: ULong): Boolean =
        prefs(context).edit().putString(key(account, channelId), epoch.toString()).commit()

    public fun clear(context: Context, account: String, channelId: String) {
        prefs(context).edit().remove(key(account, channelId)).commit()
    }

    /** The account wipe. */
    public fun forgetAccount(context: Context, account: String) {
        val p = prefs(context)
        val edit = p.edit()
        p.all.keys.filter { it.startsWith("deleted|$account|") }.forEach(edit::remove)
        edit.commit()
    }
}

/**
 * Security pass F3 - whether a group joined while this device holds none
 * for the channel stands: always when no group was deleted before
 * ([deletedEpoch] `null`) or the channel is under reset suspicion (a
 * re-founded chain restarts low); otherwise only above the deleted group's
 * epoch. Pure.
 */
public fun joinWithoutHeldGroupAllowed(joinedEpoch: ULong, deletedEpoch: ULong?, resetSuspected: Boolean): Boolean =
    deletedEpoch == null || resetSuspected || joinedEpoch > deletedEpoch
