// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context

/**
 * ADR-0090 decision A — a channel's MLS epoch and its channel-service epoch
 * are no longer one number: a re-founded PublisherGroup starts again at MLS
 * epoch 0 while the channels service is at N. Per (account, channel) this
 * device keeps `channel epoch - MLS epoch`: set by a re-founding (the epoch
 * its reseal moved the service to), and learned by a device that joins the
 * group (an admission or a co-publisher approve), whose Welcome comes after
 * the committing device's reseal, from the service epoch at landing. `0`
 * (nothing recorded) is the case of every channel never re-founded.
 */
public object ChannelEpochOffsets {
    private const val PREFS = "lakko.channels.epoch-offsets"

    /** The bound account (set by [ChannelsUiState.bind]); `null` reads 0. */
    @Volatile
    public var boundAccount: String? = null

    private fun prefs(context: Context) = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun key(account: String, channelId: String) = "offset|$account|${channelId.lowercase()}"

    /** The offset of [channelId]; 0 when none is recorded. */
    public fun get(channelId: String): Long {
        val context = SubscriberAdmission.appContext ?: return 0L
        val account = boundAccount ?: return 0L
        return prefs(context).getLong(key(account, channelId), 0L)
    }

    /** Record the offset of [channelId]. */
    public fun set(channelId: String, offset: Long) {
        val context = SubscriberAdmission.appContext ?: return
        val account = boundAccount ?: return
        prefs(context).edit().putLong(key(account, channelId), offset).apply()
    }

    /** The channel epoch the MLS epoch [groupEpoch] serves. */
    public fun channelEpochOf(channelId: String, groupEpoch: ULong): UInt? =
        channelEpochFor(groupEpoch, get(channelId))

    /** Learn the offset at a join: the service's epoch now, minus the group's. */
    public fun learn(channelId: String, groupEpoch: ULong, serviceEpoch: UInt) {
        set(channelId, offsetFor(serviceEpoch, groupEpoch))
    }

    /** Forget device. */
    public fun forgetAccount(context: Context, account: String) {
        val p = prefs(context)
        val editor = p.edit()
        p.all.keys.filter { it.startsWith("offset|$account|") }.forEach(editor::remove)
        editor.apply()
    }
}

/** `channel epoch - MLS epoch` when the service is at [serviceEpoch] and the
 *  group at [groupEpoch]. Pure. */
internal fun offsetFor(serviceEpoch: UInt, groupEpoch: ULong): Long = serviceEpoch.toLong() - groupEpoch.toLong()

/** The channel epoch [groupEpoch] serves under [offset], or `null` outside
 *  the channel epoch range. Pure. */
internal fun channelEpochFor(groupEpoch: ULong, offset: Long): UInt? {
    val e = groupEpoch.toLong() + offset
    return if (e < 0 || e > UInt.MAX_VALUE.toLong()) null else e.toUInt()
}

/**
 * 2026-11-20 — the offset that makes the mapping hold again, or `null` when
 * it holds. Each owner commit is meant to move the service by one epoch with
 * its reseal, so the group is never more than one epoch ahead of the
 * service; a run of commits whose reseal never landed (seen live: about 150
 * rotations refused `401`) leaves it far ahead, and the mapping then names
 * service epochs that will never carry its seed. The reseal that does land
 * seals the group's CURRENT seed at [serviceEpoch] + 1, so that is where the
 * current MLS epoch maps. Pure.
 */
internal fun reanchoredOffset(groupEpoch: ULong, offset: Long, serviceEpoch: UInt): Long? {
    val next = serviceEpoch.toLong() + 1
    if (groupEpoch.toLong() + offset <= next) return null
    return next - groupEpoch.toLong()
}

