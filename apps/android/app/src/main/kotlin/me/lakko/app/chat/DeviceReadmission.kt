// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.chat

import android.content.Context

/**
 * ADR-0050 section 2.4 / 2.6 — the re-admission arm, and its mirror.
 *
 * Mirror of `apps/web/src/chat/device-readmission.ts`.
 *
 * A person's device that has published a KeyPackage but holds no leaf in a
 * chat the person is in should be added: a device restored by three words with
 * no other device online, a device whose pairing fan-out was cut short, a
 * contact's new phone. Any member may propose it (the core's second
 * self-service arm), so every client re-checks, on a bounded schedule, the
 * device list of each account in each chat against that chat's roster.
 *
 * The mirror: a leaf of THIS account whose device no longer has a directory row
 * (`forgetDevice` deletes its row before wiping) is retired, by this account's
 * own devices only — a plain member may not remove another account's leaf.
 *
 * What the planner cannot know is why a device is absent from a roster: never
 * added, or removed by an admin. A device this client SAW removed from the chat
 * is remembered per chat and never re-admitted by it; a removal that happened
 * before this client joined is invisible to it, and the device would be
 * re-admitted — recorded in the ADR as the arm's known limit.
 *
 * Keys are opaque strings to the planner; the caller uses one encoding for
 * roster leaves, directory devices and the removed memory.
 */
public object DeviceReadmission {

    /** How often one chat is re-checked. */
    public const val INTERVAL_MS: Long = 10L * 60_000L

    /** A device row from the directory, as fetched. */
    public class DirectoryDevice(public val deviceKey: String, public val keyPackage: ByteArray)

    /** A roster leaf with the person it resolves to. */
    public data class RosterLeaf(val leafKey: String, val personKey: String)

    /** One account in the chat with its directory list; `null` devices when the
     *  fetch failed — nothing is decided for that account. */
    public class AccountDevices(public val personKey: String, public val devices: List<DirectoryDevice>?)

    /** What to do: KeyPackages to propose an Add for, own leaf keys to retire. */
    public class Plan(public val admit: List<DirectoryDevice>, public val retire: List<String>)

    public fun plan(
        roster: List<RosterLeaf>,
        ownPersonKey: String,
        accounts: List<AccountDevices>,
        removed: Set<String>,
    ): Plan {
        val rosterKeys = roster.map { it.leafKey }.toSet()
        val admit = mutableListOf<DirectoryDevice>()
        val retire = mutableListOf<String>()
        val seen = HashSet<String>()
        for (account in accounts) {
            val devices = account.devices ?: continue
            val listed = devices.map { it.deviceKey }.toSet()
            for (d in devices) {
                if (d.deviceKey in rosterKeys) continue
                if (d.deviceKey in removed) continue
                if (!seen.add(d.deviceKey)) continue
                admit += d
            }
            if (account.personKey != ownPersonKey) continue
            for (leaf in roster) {
                if (leaf.personKey != ownPersonKey) continue
                if (leaf.leafKey in listed) continue
                retire += leaf.leafKey
            }
        }
        return Plan(admit, retire)
    }

    // ---- the throttle -----------------------------------------------------

    private val lastCheckedAt = java.util.concurrent.ConcurrentHashMap<String, Long>()

    /** Whether [groupIdHex] is due for a check at [nowMs]; stamps it when so. */
    public fun takeSlot(groupIdHex: String, nowMs: Long): Boolean {
        synchronized(lastCheckedAt) {
            val last = lastCheckedAt[groupIdHex]
            if (last != null && nowMs - last < INTERVAL_MS) return false
            lastCheckedAt[groupIdHex] = nowMs
            return true
        }
    }

    /** Sign-out and tests. */
    public fun clearSlots() {
        lastCheckedAt.clear()
    }

    // ---- the removed-device memory -----------------------------------------

    private const val PREFS = "lakko-readmission-removed"

    /** The leaves that left between two roster reads; a torn read on either
     *  side records nothing. Pure, for tests. */
    public fun departed(before: List<String>?, after: List<String>?): List<String> {
        if (before == null || after == null) return emptyList()
        val now = after.toSet()
        return before.filterNot { it in now }
    }

    /** Remember [before] minus [after] for [groupIdHex]. Best-effort. */
    public fun recordRemoved(
        context: Context,
        accountIdB64: String,
        groupIdHex: String,
        before: List<String>?,
        after: List<String>?,
    ) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            val gone = departed(before, after)
            if (gone.isEmpty()) return
            try {
                val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                val key = "$accountIdB64|$groupIdHex"
                val cur = prefs.getStringSet(key, emptySet()).orEmpty().toMutableSet()
                cur += gone
                prefs.edit().putStringSet(key, cur).apply()
            } catch (cause: Throwable) {
                android.util.Log.w("DeviceReadmission", "removed-device memory not written", cause)
            }
    
        }
    }

    public fun removed(context: Context, accountIdB64: String, groupIdHex: String): Set<String> =
        try {
            context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
                .getStringSet("$accountIdB64|$groupIdHex", emptySet())
                .orEmpty()
                .toSet()
        } catch (_: Throwable) {
            emptySet()
        }

    public fun clearForAccount(context: Context, accountIdB64: String) {
        try {
            val prefs = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
            val editor = prefs.edit()
            for (key in prefs.all.keys) {
                if (key.startsWith("$accountIdB64|")) editor.remove(key)
            }
            editor.apply()
        } catch (_: Throwable) {
            // nothing to clear
        }
    }
}
