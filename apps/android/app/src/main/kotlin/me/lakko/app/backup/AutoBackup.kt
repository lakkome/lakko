// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.backup

import android.content.Context
import android.content.SharedPreferences
import org.json.JSONObject

/**
 * Keep the encrypted backup current.
 *
 * Until this existed the backup was written exactly twice in an account's
 * life - an empty baseline at signup and one more at recover - so everything a
 * person did afterwards (contacts accepted, groups joined, renames) never
 * reached the server blob, and a restore on a new device landed them back at
 * registration day. [BackupClient.uploadFromCache] was built for precisely
 * this and had no caller anywhere.
 *
 * Mirror of the web `backup/auto-backup.ts` and the iOS `AutoBackup.swift`.
 * The rules live here, pure and clock-free, so they can be run by a test
 * rather than observed on a device.
 */
public object AutoBackup {

    /** No more than one upload per this interval, however much changes. */
    public const val MIN_INTERVAL_MS: Long = 5 * 60 * 1000

    /**
     * How often the driver asks. A tick over a clean account reads two
     * integers and returns, so this can be short enough to catch a change soon
     * after it lands without costing anything when nothing has.
     */
    public const val TICK_MS: Long = 60_000

    /** What the driver remembers between runs, and across launches. */
    public data class State(
        /** When the last upload succeeded. `0` means never. */
        val lastUploadAtMs: Long = 0,
        /** When the account first became worth backing up again, or `null`
         *  when nothing has changed since the last upload. */
        val dirtySinceMs: Long? = null,
        /** The group set this device last reported, as a sorted join. `null`
         *  before the first report - which counts as a change, because nothing
         *  here can vouch for what the server already holds. */
        val lastGroupsKey: String? = null,
    ) {
        public companion object {
            public val CLEAN: State = State()
        }
    }

    public enum class Verdict { CLEAN, THROTTLED, DUE }

    /**
     * Decide whether a backup should go out now.
     *
     * A clock that has moved BACKWARDS reads as due, which is the opposite of
     * the choice `should_request_profiles` makes for its own throttle, and
     * deliberately so: there, suppressing costs a display name; here,
     * suppressing costs the user's data, and a stored [State.lastUploadAtMs]
     * in the future would otherwise wedge backups off forever.
     */
    public fun verdict(
        state: State,
        nowMs: Long,
        minIntervalMs: Long = MIN_INTERVAL_MS,
    ): Verdict {
        if (state.dirtySinceMs == null) return Verdict.CLEAN
        if (state.lastUploadAtMs == 0L) return Verdict.DUE
        val elapsed = nowMs - state.lastUploadAtMs
        if (elapsed < 0) return Verdict.DUE
        return if (elapsed < minIntervalMs) Verdict.THROTTLED else Verdict.DUE
    }

    /**
     * The state after a change worth backing up. Idempotent: the FIRST such
     * change sets the clock, so the throttle is measured from the last upload
     * and not pushed out by every later edit.
     */
    public fun markedDirty(state: State, nowMs: Long): State =
        if (state.dirtySinceMs != null) state else state.copy(dirtySinceMs = nowMs)

    /**
     * The state after a successful upload: the clock restarts and the account
     * is clean again. The group key survives - it describes what was reported,
     * not what was uploaded.
     */
    public fun markedUploaded(state: State, nowMs: Long): State =
        state.copy(lastUploadAtMs = nowMs, dirtySinceMs = null)

    /**
     * The canonical form of a group set.
     *
     * Sorted, because the order a repository hands its threads back in is not
     * guaranteed stable and an unsorted comparison would report a change
     * whenever the list was merely reordered.
     */
    public fun groupsKey(groupIds: List<String>): String = groupIds.sorted().joinToString(",")
}

/**
 * `SharedPreferences`, keyed per account.
 *
 * Two integers and a short string - no phrase, no key, no envelope - so this
 * needs neither the Keystore nor the encrypted store.
 */
public object AutoBackupStore {

    private const val PREFS = "lakko.backup.auto"

    private fun prefs(context: Context): SharedPreferences =
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    public fun load(context: Context, accountIdB64: String): AutoBackup.State {
        val raw = prefs(context).getString(accountIdB64, null) ?: return AutoBackup.State.CLEAN
        return runCatching {
            val o = JSONObject(raw)
            AutoBackup.State(
                lastUploadAtMs = o.optLong("lastUploadAtMs", 0),
                dirtySinceMs = if (o.has("dirtySinceMs") && !o.isNull("dirtySinceMs")) {
                    o.getLong("dirtySinceMs")
                } else {
                    null
                },
                lastGroupsKey = if (o.has("lastGroupsKey") && !o.isNull("lastGroupsKey")) {
                    o.getString("lastGroupsKey")
                } else {
                    null
                },
            )
        }.getOrDefault(AutoBackup.State.CLEAN)
    }

    public fun save(context: Context, accountIdB64: String, state: AutoBackup.State) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val o = JSONObject()
                .put("lastUploadAtMs", state.lastUploadAtMs)
                .put("dirtySinceMs", state.dirtySinceMs ?: JSONObject.NULL)
                .put("lastGroupsKey", state.lastGroupsKey ?: JSONObject.NULL)
            prefs(context).edit().putString(accountIdB64, o.toString()).apply()
    
        }
    }

    public fun clear(context: Context, accountIdB64: String) {
        prefs(context).edit().remove(accountIdB64).apply()
    }

    /** Record that something worth backing up has changed. */
    public fun markDirty(context: Context, accountIdB64: String, nowMs: Long) {
        val state = load(context, accountIdB64)
        val next = AutoBackup.markedDirty(state, nowMs)
        if (next != state) save(context, accountIdB64, next)
    }

    /**
     * Whether the set of groups this device knows about has changed since the
     * last time this was asked, recording the new set as it goes.
     *
     * Persisted rather than held in memory, because an in-memory memo starts
     * empty on every launch and would therefore report a change at every
     * launch - turning "back up when membership moves" into "back up once per
     * session", which is a different rule wearing the same name.
     */
    public fun knownGroupsChanged(
        context: Context,
        accountIdB64: String,
        groupIds: List<String>,
    ): Boolean {
        val key = AutoBackup.groupsKey(groupIds)
        val state = load(context, accountIdB64)
        if (state.lastGroupsKey == key) return false
        save(context, accountIdB64, state.copy(lastGroupsKey = key))
        return true
    }
}
