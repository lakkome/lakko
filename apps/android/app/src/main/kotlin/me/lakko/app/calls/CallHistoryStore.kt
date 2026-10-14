// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.calls

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * The per-account cross-conversation call index — `docs/ux/navigation.md`
 * section 6.2.
 *
 * ## Why a store at all
 *
 * The Calls screen wants the opposite cut from the conversation: one sequence
 * in time order regardless of who the call was with, because a person looking
 * for a call remembers roughly when, not which thread it belonged to. Deriving
 * that from the transcripts means walking every conversation on every open, and
 * the spec names that as the thing not to do: the list reads an index written
 * when the call row is.
 *
 * So one record is appended here by
 * [me.lakko.app.chat.ChatRepository.recordCallRow], beside the transcript row
 * it already writes. The two are written together and the transcript remains
 * the record of record: if this index is ever lost or unreadable, the calls are
 * still in their conversations and only this screen is empty.
 *
 * ## Why encrypted
 *
 * Who you called and when is exactly the metadata the whole system is built to
 * keep off a server, so keeping it in plaintext on the device would be a
 * downgrade from the encrypted message log the same facts already live in.
 * [EncryptedSharedPreferences], the posture [me.lakko.app.contacts.PinStore]
 * and [me.lakko.app.push.PushRoutingSecretStore] already use. Nothing here
 * needs to be readable by a background process without the device key, which is
 * what makes the plaintext exception behind [me.lakko.app.push.PushRouteStore]
 * inapplicable.
 *
 * ## Storage layout
 *
 * One `EncryptedSharedPreferences` file, one entry per account, keyed
 * `"calls-v1:<accountIdB64>"`. The value is the records of
 * [encodeCallHistoryEntry] joined by newlines, newest first, capped at
 * [CALL_HISTORY_MAX]. A line this build cannot read is dropped on load rather
 * than failing the read.
 */
public object CallHistoryStore {
    private const val PREFS_NAME = "lakko-call-history"
    private const val KEY_PREFIX = "calls-v1:"
    private const val TAG = "CallHistoryStore"

    private val changed = MutableStateFlow(0)

    /**
     * Bumped on every write, so an open Calls screen redraws.
     *
     * A counter rather than the list itself: the list is per account and the
     * screen already knows which account it is showing, so handing it a value
     * it would have to filter would only be a second place for the two to
     * disagree.
     */
    public val version: StateFlow<Int> = changed.asStateFlow()

    /** The calls of [accountIdB64], newest first. Empty when there are none. */
    public fun load(context: Context, accountIdB64: String?): List<CallHistoryEntry> {
        if (accountIdB64.isNullOrEmpty()) return emptyList()
        val raw = prefs(context)?.getString(key(accountIdB64), null) ?: return emptyList()
        if (raw.isEmpty()) return emptyList()
        return callHistoryOrdered(
            raw.split('\n').mapNotNull { line ->
                line.takeIf { it.isNotBlank() }?.let(::decodeCallHistoryEntry)
            },
        )
    }

    /**
     * Append one finished call.
     *
     * Best-effort and never throwing: this runs on the tail of a call teardown,
     * and failing a teardown to report that a list entry could not be filed
     * would trade something that matters for something that does not.
     */
    public fun record(context: Context, accountIdB64: String?, entry: CallHistoryEntry) {
        if (accountIdB64.isNullOrEmpty()) return
        val store = prefs(context) ?: return
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            try {
                val next = callHistoryWith(load(context, accountIdB64), entry)
                store.edit()
                    .putString(key(accountIdB64), next.joinToString("\n", transform = ::encodeCallHistoryEntry))
                    .apply()
                changed.value += 1
            } catch (cause: Throwable) {
                android.util.Log.w(TAG, "recording the call history entry failed", cause)
            }
        }
    }

    /**
     * Fill the index from calls that happened before it existed. Once.
     *
     * ## Why this is not the scan the spec forbids
     *
     * Section 6.2 refuses a walk of every transcript **on every open**, because
     * that cost grows with every call ever made. This walk happens at most once
     * per account, ever, and what it produces is the index the screen then
     * reads. Without it the Calls screen opens empty on a device that has been
     * calling for months, while those same calls are visibly sitting in the
     * conversation list - which reads as a broken screen, and is the exact
     * thing a live pass found that the unit tests could not.
     *
     * [entries] is what the caller already holds in memory. It is asked for
     * lazily, so an account that has already been filled never pays for
     * gathering it.
     *
     * Marked done even when it finds nothing: an account with no past calls is
     * filled correctly by writing nothing, and re-walking on every launch to
     * rediscover that would be the per-open cost in a slower disguise.
     */
    public fun backfillOnce(
        context: Context,
        accountIdB64: String?,
        entries: () -> List<CallHistoryEntry>,
    ) {
        if (accountIdB64.isNullOrEmpty()) return
        val store = prefs(context) ?: return
        if (store.getBoolean(backfilledKey(accountIdB64), false)) return
        try {
            val found = entries()
            if (found.isNotEmpty()) {
                // Fold one at a time through the same ordering and cap the
                // live path uses, so a backfilled index is indistinguishable
                // from one that was written call by call.
                var next = load(context, accountIdB64)
                for (entry in found) next = callHistoryWith(next, entry)
                store.edit()
                    .putString(
                        key(accountIdB64),
                        next.joinToString("\n", transform = ::encodeCallHistoryEntry),
                    )
                    .putBoolean(backfilledKey(accountIdB64), true)
                    .apply()
                changed.value += 1
                return
            }
            store.edit().putBoolean(backfilledKey(accountIdB64), true).apply()
        } catch (cause: Throwable) {
            android.util.Log.w(TAG, "backfilling the call history failed", cause)
        }
    }

    /** Drop one account's index (forget device, sign out of that account). */
    public fun clear(context: Context, accountIdB64: String?) {
        if (accountIdB64.isNullOrEmpty()) return
        val store = prefs(context) ?: return
        try {
            // The backfill marker goes with the index. Otherwise a cleared
            // account would come back empty for good, the walk having been
            // recorded as already done.
            store.edit()
                .remove(key(accountIdB64))
                .remove(backfilledKey(accountIdB64))
                .apply()
            changed.value += 1
        } catch (cause: Throwable) {
            android.util.Log.w(TAG, "clearing the call history failed", cause)
        }
    }

    private fun key(accountIdB64: String): String = KEY_PREFIX + accountIdB64

    private fun backfilledKey(accountIdB64: String): String =
        KEY_PREFIX + "backfilled." + accountIdB64

    @Volatile
    private var opened: SharedPreferences? = null

    /**
     * `null` when the encrypted file cannot be opened at all — a Keystore that
     * the OS invalidated, say. The Calls screen then shows its empty state,
     * which is wrong but harmless; the calls themselves are in their
     * conversations either way.
     *
     * Held once rather than per call: opening an encrypted file does Keystore
     * work, and the Calls screen reads it from a composition. A failed open is
     * NOT cached, so a Keystore that comes back after a device unlock is picked
     * up on the next read instead of being written off for the session.
     */
    private fun prefs(context: Context): SharedPreferences? {
        opened?.let { return it }
        return synchronized(this) {
            opened ?: try {
                val app = context.applicationContext
                val master = MasterKey.Builder(app)
                    .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
                    .build()
                EncryptedSharedPreferences.create(
                    app,
                    PREFS_NAME,
                    master,
                    EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
                    EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
                ).also { opened = it }
            } catch (cause: Throwable) {
                android.util.Log.w(TAG, "opening the call history store failed", cause)
                null
            }
        }
    }
}
