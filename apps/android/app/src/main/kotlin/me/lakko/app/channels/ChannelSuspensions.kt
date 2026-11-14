// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context

/**
 * ADR-0090 section 2.5, security pass - channels whose PublisherGroup journal
 * this device suspects was reset under it (refused signed reads, a DS that
 * names a newest epoch below the group's). Neither signal is authenticated,
 * so the group state is KEPT: the device only stops posting and rotating
 * under the old seed and reads, with the waiting line, until a PublisherGroup
 * Welcome for the same group id arrives signed by a device of the owner
 * (then the held group is replaced, [CoPublisher.joinReplacingHeld]). Per
 * account, plain preferences (a channel id is no secret); removed by the
 * account wipe.
 */
public object ChannelSuspensions {
    private const val PREFS = "lakko.channels.suspended"

    private fun prefs(context: Context) = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun key(account: String) = "suspended|$account"

    public fun suspend(context: Context, account: String, channelId: String) {
        val set = prefs(context).getStringSet(key(account), emptySet()).orEmpty() + channelId.lowercase()
        prefs(context).edit().putStringSet(key(account), set).apply()
    }

    public fun isSuspended(context: Context, account: String, channelId: String): Boolean =
        channelId.lowercase() in prefs(context).getStringSet(key(account), emptySet()).orEmpty()

    /** The owner-signed Welcome replaced the group: the device posts again. */
    public fun clear(context: Context, account: String, channelId: String) {
        val set = prefs(context).getStringSet(key(account), emptySet()).orEmpty() - channelId.lowercase()
        prefs(context).edit().putStringSet(key(account), set).apply()
    }

    /** Every suspended channel of [account] (lower-case hex). */
    public fun all(context: Context, account: String): Set<String> =
        prefs(context).getStringSet(key(account), emptySet()).orEmpty()

    /** Whether [channelId] is suspended for the account bound now (the
     *  session's context); `false` when either is unknown. */
    public fun isSuspendedNow(channelId: String): Boolean {
        val context = SubscriberAdmission.appContext ?: return false
        val account = ChannelEpochOffsets.boundAccount ?: return false
        return isSuspended(context, account, channelId)
    }

    public fun forgetAccount(context: Context, account: String) {
        prefs(context).edit().remove(key(account)).apply()
    }
}

/**
 * Security pass - a PublisherGroup Welcome may replace a group this device
 * already holds under [expectedGroupId] only when it is for that group and its
 * signer verifies as a device of [expectedOwnerKey] (the key the owner seed
 * gives on an owner device, the register's recorded key on a co-publisher's).
 * Pure.
 */
public fun welcomeReplacesHeldGroup(
    welcomeGroupId: ByteArray,
    expectedGroupId: ByteArray,
    founderAccountKey: ByteArray,
    expectedOwnerKey: ByteArray,
): Boolean =
    expectedOwnerKey.size == 32 &&
        welcomeGroupId.contentEquals(expectedGroupId) &&
        founderAccountKey.contentEquals(expectedOwnerKey)

/** Signed reads in a row that must succeed at the held group's epoch before a
 *  suspension is lifted (chosen, not measured). */
public const val SUSPENSION_LIFT_READS: Int = 2

/** What one probe of a suspended channel decides. */
public data class SuspensionProbe(
    /** Successful matching reads in a row after this probe. */
    val consecutiveOk: Int,
    /** Lift the suspension now: the device posts again under the held group. */
    val lift: Boolean,
)

/**
 * Security pass follow-up - a suspected reset was a false alarm when signed
 * reads with the held group's epoch key succeed again ([readOk]) AND the DS
 * names the held group's epoch as its newest ([latestEpoch] ==
 * [groupEpoch]), [SUSPENSION_LIFT_READS] times in a row. A refused read, an
 * absent epoch or one that differs (a lower one included) starts the count
 * again and keeps the suspension. Pure.
 */
public fun suspensionProbe(
    previousOk: Int,
    readOk: Boolean,
    latestEpoch: ULong?,
    groupEpoch: ULong,
): SuspensionProbe {
    if (!readOk || latestEpoch == null || latestEpoch != groupEpoch) return SuspensionProbe(0, false)
    val ok = previousOk + 1
    return SuspensionProbe(ok, ok >= SUSPENSION_LIFT_READS)
}

/**
 * Security pass follow-up - replace a held PublisherGroup by joining a
 * verified Welcome without losing both: [snapshot] the provider, [delete] the
 * held group, [join]; when the join throws, [restore] the snapshot (the held
 * group is back, the suspension stays) and rethrow. [wipe] clears the
 * snapshot either way. Runs inside one provider-lock region.
 */
public fun <S, T> replaceHeldThenJoin(
    snapshot: () -> S,
    delete: () -> Unit,
    join: () -> T,
    restore: (S) -> Unit,
    wipe: (S) -> Unit = {},
): T {
    val before = snapshot()
    try {
        delete()
        return join()
    } catch (e: Throwable) {
        restore(before)
        throw e
    } finally {
        wipe(before)
    }
}
