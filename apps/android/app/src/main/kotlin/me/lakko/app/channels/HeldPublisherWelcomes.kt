// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject

/**
 * Walk follow-up (2026-11-14), the replace rule's part B - a PublisherGroup
 * Welcome that arrived BEFORE this device applied the commit removing its
 * leaf. The account mailbox can deliver the owner device's re-admission
 * Welcome while the channel journal still has the removal unread: the held
 * group is then active and unsuspected, rule 1 ([heldGroupReplaceable])
 * ignores the Welcome, and the mailbox cursor moves past it. The owner device
 * does not send it again, so it is kept here (only when it names the held
 * group and its founder verifies as a device of the owner account) and tried
 * again on every publisher pass; once the held group is inactive or
 * suspected, the normal replace rule runs with it.
 *
 * Per account, keyed by channel; at most [MAX_PER_CHANNEL] per channel,
 * newest kept. Dropped when a Welcome for the channel joined, when the
 * replace rule ran on it and refused, when the channel was left or ended,
 * and after [MAX_AGE_MS]. The core does not report a Welcome's epoch, so
 * "the device publishes again at or above the Welcome's epoch" cannot be
 * read sooner than a join; the age bound covers a device that simply stays
 * active. A Welcome is ciphertext sealed to this device's KeyPackage: plain
 * preferences, removed by the account wipe.
 */
public object HeldPublisherWelcomes {
    /** Held Welcomes per channel (chosen, not measured). */
    public const val MAX_PER_CHANNEL: Int = 3

    /** How long a held Welcome is kept while the held group stays active
     *  (chosen, not measured): seven days. */
    public const val MAX_AGE_MS: Long = 7L * 24L * 60L * 60L * 1_000L

    /** One held Welcome. */
    public data class Held(val welcomeB64: String, val heldAtMs: Long)

    private const val PREFS = "lakko.channels.held-welcomes"

    private fun prefs(context: Context) = context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun key(account: String) = "held|$account"

    private fun loadAll(context: Context, account: String): MutableMap<String, List<Held>> {
        val raw = prefs(context).getString(key(account), null) ?: return mutableMapOf()
        return try {
            val o = JSONObject(raw)
            val out = mutableMapOf<String, List<Held>>()
            for (channel in o.keys()) {
                val arr = o.getJSONArray(channel)
                out[channel] = (0 until arr.length()).map { i ->
                    val e = arr.getJSONObject(i)
                    Held(e.getString("w"), e.getLong("at"))
                }
            }
            out
        } catch (_: org.json.JSONException) {
            mutableMapOf()
        }
    }

    private fun saveAll(context: Context, account: String, all: Map<String, List<Held>>) {
        val o = JSONObject()
        for ((channel, list) in all) {
            if (list.isEmpty()) continue
            o.put(channel, JSONArray().apply { list.forEach { put(JSONObject().put("w", it.welcomeB64).put("at", it.heldAtMs)) } })
        }
        // commit, not apply: the mailbox cursor moves past the Welcome once
        // this returns.
        prefs(context).edit().putString(key(account), o.toString()).commit()
    }

    /** Keep [welcome] for [channelId]. `false` when it did not reach the disk. */
    public fun hold(context: Context, account: String, channelId: String, welcome: ByteArray, nowMs: Long = System.currentTimeMillis()): Boolean {
        val all = loadAll(context, account)
        val id = channelId.lowercase()
        val b64 = java.util.Base64.getEncoder().encodeToString(welcome)
        val current = all[id].orEmpty()
        val next = heldWelcomesAfterAdd(current, b64, nowMs)
        if (next == current) return true
        all[id] = next
        return runCatching { saveAll(context, account, all) }.isSuccess
    }

    /** The Welcomes held for [channelId], newest first. */
    public fun heldFor(context: Context, account: String, channelId: String): List<Held> =
        loadAll(context, account)[channelId.lowercase()].orEmpty().sortedByDescending { it.heldAtMs }

    /** Every channel with a held Welcome (lower-case hex). */
    public fun channels(context: Context, account: String): Set<String> = loadAll(context, account).keys

    /** Drop one held Welcome. */
    public fun drop(context: Context, account: String, channelId: String, welcomeB64: String) {
        val all = loadAll(context, account)
        val id = channelId.lowercase()
        all[id] = all[id].orEmpty().filterNot { it.welcomeB64 == welcomeB64 }
        saveAll(context, account, all)
    }

    /** Drop every held Welcome of [channelId]. */
    public fun clear(context: Context, account: String, channelId: String) {
        val all = loadAll(context, account)
        if (all.remove(channelId.lowercase()) != null) saveAll(context, account, all)
    }

    /** The account wipe. */
    public fun forgetAccount(context: Context, account: String) {
        prefs(context).edit().remove(key(account)).apply()
    }
}

/** The held list after [welcomeB64] arrived at [nowMs]: the same Welcome is
 *  kept once (its first time), at most [HeldPublisherWelcomes.MAX_PER_CHANNEL],
 *  the newest kept. Pure. */
public fun heldWelcomesAfterAdd(held: List<HeldPublisherWelcomes.Held>, welcomeB64: String, nowMs: Long): List<HeldPublisherWelcomes.Held> {
    if (held.any { it.welcomeB64 == welcomeB64 }) return held
    return (held + HeldPublisherWelcomes.Held(welcomeB64, nowMs))
        .sortedBy { it.heldAtMs }
        .takeLast(HeldPublisherWelcomes.MAX_PER_CHANNEL)
}

/** What the publisher pass does with one held Welcome after trying it. */
public enum class HeldWelcomeStep {
    /** It joined: every held Welcome of the channel goes. */
    JOINED_CLEAR_CHANNEL,

    /** The held group is still active and unsuspected: kept for later. */
    KEEP,

    /** The replace rule ran on it and refused (or nothing is held any
     *  more), or it is too old: it goes. */
    DROP,
}

/** Pure - the step for a held Welcome tried at [nowMs]. [joined]: the try
 *  landed. [stillWaiting]: the held group is still active and unsuspected
 *  (the removal is not applied yet). */
public fun heldWelcomeStep(joined: Boolean, stillWaiting: Boolean, heldAtMs: Long, nowMs: Long): HeldWelcomeStep = when {
    joined -> HeldWelcomeStep.JOINED_CLEAR_CHANNEL
    heldWelcomeExpired(heldAtMs, nowMs) -> HeldWelcomeStep.DROP
    stillWaiting -> HeldWelcomeStep.KEEP
    else -> HeldWelcomeStep.DROP
}

/** Whether a Welcome held at [heldAtMs] is older than
 *  [HeldPublisherWelcomes.MAX_AGE_MS] at [nowMs]; a clock moved backwards
 *  keeps it. The same rule as iOS `heldWelcomeExpired`. Pure. */
public fun heldWelcomeExpired(heldAtMs: Long, nowMs: Long): Boolean =
    nowMs > heldAtMs && nowMs - heldAtMs > HeldPublisherWelcomes.MAX_AGE_MS
