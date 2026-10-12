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
 * For the OWN account that limit is closed (ADR-0066 section 2.3): a device of
 * this account is admitted only while the account's DeviceGroup roster lists
 * it. A removed device is not in that roster, whatever it publishes.
 *
 * Keys are opaque strings to the planner; the caller uses one encoding for
 * roster leaves, directory devices and the removed memory.
 */
public object DeviceReadmission {

    /** How often one chat is re-checked. */
    public const val INTERVAL_MS: Long = 10L * 60_000L

    /** A device row from the directory, as fetched. */
    public class DirectoryDevice(
        public val deviceKey: String,
        public val keyPackage: ByteArray,
        /** ADR-0066 (a device paired after a key rotation): `false` when the
         *  listed KeyPackage of an OWN device is bound to another account key
         *  than this account's conversation key ([ownKeyPackageUsable]). The
         *  row still counts as listed (its leaf is not retired); it is not
         *  admitted until the device publishes one bound to the right key. */
        public val admissible: Boolean = true,
    )

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
        /**
         * ADR-0066 section 2.3 — the device keys in THIS account's
         * DeviceGroup roster, or `null` when the roster could not be read.
         * A device of the own account is admitted only when it is in it: the
         * DeviceGroup is the account's own, MLS-authenticated record of its
         * devices, and a removed device is not in it, so a device that
         * publishes itself again after its removal is not put back. An
         * unreadable roster admits no own device (fail closed). Contacts'
         * devices are not filtered by it; [removed] stays for every account
         * as defence in depth.
         */
        ownDeviceRoster: Set<String>?,
    ): Plan {
        val rosterKeys = roster.map { it.leafKey }.toSet()
        val admit = mutableListOf<DirectoryDevice>()
        val retire = mutableListOf<String>()
        val seen = HashSet<String>()
        for (account in accounts) {
            val devices = account.devices ?: continue
            val listed = devices.map { it.deviceKey }.toSet()
            val own = account.personKey == ownPersonKey
            for (d in devices) {
                if (d.deviceKey in rosterKeys) continue
                if (d.deviceKey in removed) continue
                if (own && !ownDeviceAdmissible(d.deviceKey, ownDeviceRoster)) continue
                if (own && !d.admissible) continue
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

    /**
     * Whether a device of the OWN account may be added to a conversation:
     * only when the account's DeviceGroup roster lists its key. `null`
     * roster (not readable now) admits nothing. The same rule guards the
     * pairing fan-out (`ChatRepository.admitDevice`).
     */
    public fun ownDeviceAdmissible(deviceKey: String, ownDeviceRoster: Set<String>?): Boolean =
        ownDeviceRoster != null && deviceKey in ownDeviceRoster

    /**
     * ADR-0066 - whether a KeyPackage an OWN device published may be used to
     * admit it to a conversation: its device binding must name the account
     * key this account's conversation leaves are bound to ([conversationKey],
     * the earliest own predecessor after a rotation). A device paired after a
     * rotation learns that key from the pairing state bundle; a KeyPackage it
     * published before the bundle arrived names the CURRENT key, and a leaf
     * admitted with it would read as another account in the chat. An unbound
     * KeyPackage ([boundAccountKey] `null`) and an unknown conversation key
     * change nothing.
     */
    public fun ownKeyPackageUsable(boundAccountKey: ByteArray?, conversationKey: ByteArray?): Boolean =
        boundAccountKey == null || conversationKey == null || boundAccountKey.contentEquals(conversationKey)

    // ---- ADR-0065 R.2: the wider lookup ----------------------------------

    /** A roster person this device can look up in the directory. */
    public data class LookupCandidate(
        /** The PERSON key the roster names (the key the leaves are bound to). */
        val rosterKeyB64: String,
        /** The account id this device knows that person under, base64. */
        val accountIdB64: String,
    )

    /** The slice of a contact row the lookup reads. */
    public data class LookupContact(val signaturePubkeyB64: String, val accountIdB64: String?)

    /**
     * ADR-0065 R.2 "Triggers" — every roster account whose account id this
     * device knows, not only its accepted contacts: a contact row (found by
     * its current key, or by a key an accepted reset retired for the account
     * the row names), else the profile store (the profile envelope carries
     * the account id, ADR-0048 v2). This is also what removes G-44's "no
     * admin is the person's contact" case.
     *
     * The contact row wins over the profile's claim. The own account is not
     * a candidate (the caller looks itself up with its own id). Mirror of web
     * `readmissionCandidates`; Android keeps a contact's retired keys in the
     * reset ledger rather than on the row, hence [accountOfRetiredKey].
     */
    public fun readmissionCandidates(
        rosterPersonKeysB64: List<String>,
        ownPersonKeyB64: String,
        contacts: List<LookupContact>,
        /** The account id (base64) an accepted reset retired this key for. */
        accountOfRetiredKey: (String) -> String?,
        /** The account id (base64) a member's own profile claims, or `null`. */
        profileAccountIdB64Of: (String) -> String?,
    ): List<LookupCandidate> {
        val out = ArrayList<LookupCandidate>()
        val seen = HashSet<String>()
        for (rosterKey in rosterPersonKeysB64) {
            if (rosterKey == ownPersonKeyB64 || !seen.add(rosterKey)) continue
            val retiredFor = accountOfRetiredKey(rosterKey)
            val row = contacts.firstOrNull { it.signaturePubkeyB64 == rosterKey }
                ?: retiredFor?.let { id -> contacts.firstOrNull { it.accountIdB64 == id } }
            val accountId = row?.accountIdB64 ?: retiredFor ?: profileAccountIdB64Of(rosterKey) ?: continue
            if (!isAccountIdB64(accountId)) continue
            out += LookupCandidate(rosterKey, accountId)
        }
        return out
    }

    private fun isAccountIdB64(value: String): Boolean =
        try {
            java.util.Base64.getDecoder().decode(value).size == ACCOUNT_ID_LEN
        } catch (_: IllegalArgumentException) {
            false
        }

    private const val ACCOUNT_ID_LEN: Int = 16

    /** How the roster's key for a person stands against what this device
     *  trusts for the account AFTER the identity check ran. */
    public enum class LookupStanding {
        /** The roster key is the account's current key or a PROVEN
         *  predecessor of it: the ordinary ADR-0050 re-admission. */
        SAME_PERSON,

        /** The roster key was retired by an ACCEPTED RESET of the account:
         *  the directory now lists the new identity's devices (R.7 case 2). */
        RESET_IDENTITY,

        /** Anything else (no pin, an unrelated key): the account's devices
         *  are not admitted under this person. */
        SKIP,
    }

    /**
     * | pinned | roster key is ...                               | standing         |
     * |--------|-------------------------------------------------|------------------|
     * | yes    | the pin, or a proven predecessor of the pin     | `SAME_PERSON`    |
     * | yes    | a key an accepted reset of this account retired | `RESET_IDENTITY` |
     * | yes    | anything else                                   | `SKIP`           |
     * | no     | the key of this device's VERIFIED BINDING       | `SAME_PERSON`    |
     * | no     | anything else, or no binding                    | `SKIP`           |
     *
     * `SKIP` is what stops a member's own profile from attaching another
     * account's devices to itself: an account id is only followed to devices
     * when the roster's key is one this device TRUSTS for that account. A
     * claimed id alone is never followed.
     */
    public fun lookupStanding(
        rosterKeyB64: String,
        /** The pinned key and its proven predecessors; `null` = not pinned. */
        pinnedChainB64: List<String>?,
        /** The keys an accepted reset of THIS account retired. */
        supersededKeysB64: Set<String>,
        /** The key of the verified binding of the account when NOT pinned. */
        bindingKeyB64: String?,
    ): LookupStanding {
        if (pinnedChainB64 == null) {
            return if (bindingKeyB64 != null && bindingKeyB64 == rosterKeyB64) LookupStanding.SAME_PERSON else LookupStanding.SKIP
        }
        if (rosterKeyB64 in pinnedChainB64) return LookupStanding.SAME_PERSON
        return if (rosterKeyB64 in supersededKeysB64) LookupStanding.RESET_IDENTITY else LookupStanding.SKIP
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

    /** Make [groupIdHex] due at once (a claim arrived, R.6). */
    public fun clearSlot(groupIdHex: String) {
        lastCheckedAt.remove(groupIdHex)
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
