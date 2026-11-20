// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context

/**
 * ADR-0051 / ADR-0087 section 15 — the channel record a publisher owes after
 * a PublisherGroup commit (an approve, a removal, a rotation). The record is
 * issued again under the new epoch so a new publisher, and every reader let
 * in later, finds a record it can open. A failed send stays owed and the
 * publisher pass retries it until it lands; the mark is written before the
 * attempt, so an app closed mid-send still owes it.
 */
internal interface OwedRecordMarks {
    fun isOwed(channelId: String): Boolean
    fun mark(channelId: String)
    fun clear(channelId: String)
}

/**
 * Run [reissue] for [channelId] (a channel record re-issue, a rotate
 * request), owed until it reports it landed (`true`): the mark is written
 * first and cleared only on success. Returns whether it landed.
 */
internal suspend fun reissueRecordOwed(
    marks: OwedRecordMarks,
    channelId: String,
    reissue: suspend () -> Boolean,
): Boolean {
    marks.mark(channelId)
    val landed = reissue()
    if (landed) marks.clear(channelId)
    return landed
}

/**
 * The first record of a channel this device created: owed before the send,
 * cleared once it landed. A failed send is logged and stays owed, and the
 * publisher loop re-issues it ([ChannelMetaPublisher.reissue]). Returns
 * whether it landed; never throws but a cancellation.
 */
internal suspend fun postFirstChannelRecord(
    marks: OwedRecordMarks,
    channelId: String,
    post: suspend () -> Unit,
): Boolean = reissueRecordOwed(marks, channelId) {
    try {
        post()
        true
    } catch (e: kotlinx.coroutines.CancellationException) {
        throw e
    } catch (e: Throwable) {
        android.util.Log.w("ChannelRecordOwed", "channel record not posted at creation; owed", e)
        false
    }
}

/** Owed marks of the bound account, in this device's preferences file
 *  [prefsName], keyed `account|channel`. Nothing is recorded while no
 *  account is bound. Written with `commit()`: a mark is on disk before the
 *  attempt it covers. */
internal open class PrefsOwedMarks(
    context: Context,
    private val prefsName: String,
    private val account: () -> String?,
) : OwedRecordMarks {
    private val prefs = context.applicationContext.getSharedPreferences(prefsName, Context.MODE_PRIVATE)

    private fun key(channelId: String): String? = account()?.let { "$it|$channelId" }

    override fun isOwed(channelId: String): Boolean = key(channelId)?.let { prefs.getBoolean(it, false) } == true

    override fun mark(channelId: String) {
        val k = key(channelId) ?: return
        prefs.edit().putBoolean(k, true).commit()
    }

    override fun clear(channelId: String) {
        val k = key(channelId) ?: return
        prefs.edit().remove(k).commit()
    }

    internal companion object {
        /** Channel records a re-issue after a commit still owes. */
        const val RECORD_PREFS = "lakko.channels.record-owed"

        /** ADR-0087 section 15 (2026-11-08) — a co-publisher's rotate
         *  request not yet published (the subscriber row is already gone). */
        const val ROTATE_REQUEST_PREFS = "lakko.channels.rotate-request-owed"

        /** Forget device / Delete account: the account's marks of every kind go. */
        fun deleteAccount(context: Context, account: String) {
            for (name in listOf(RECORD_PREFS, ROTATE_REQUEST_PREFS)) {
                val prefs = context.applicationContext.getSharedPreferences(name, Context.MODE_PRIVATE)
                val edit = prefs.edit()
                for (k in prefs.all.keys) if (k.startsWith("$account|")) edit.remove(k)
                edit.commit()
            }
        }
    }
}

/** The channel records owed by the bound account. */
internal class PrefsOwedRecordMarks(context: Context, account: () -> String?) :
    PrefsOwedMarks(context, RECORD_PREFS, account)

/** The rotate requests the bound account (a co-publisher) still owes. */
internal class PrefsOwedRotateRequests(context: Context, account: () -> String?) :
    PrefsOwedMarks(context, ROTATE_REQUEST_PREFS, account)
