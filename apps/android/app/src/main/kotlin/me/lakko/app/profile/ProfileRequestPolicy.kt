// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.profile

import android.content.Context
import android.content.SharedPreferences

/**
 * ADR-0048 §2, trigger 4 — the "introduce yourselves" request on the Android
 * client: the wire peek, the unnamed-member count, the decision, and the
 * per-`(account, group)` "when did this device last ask" store.
 *
 * ## The problem
 *
 * A profile reaches a group by PUSH only: a device publishes its own when it
 * joins, when the name changes, and when it sees the group grow. The growth
 * re-send is what a newcomer depends on for everyone else's name, and it fires
 * on the OTHER devices — one that is away when the join lands never re-sends,
 * and the newcomer sits with `Member` for that person with nothing it can do.
 * Seen live on 2026-09-19: three of five members unnamed on the device that
 * joined last.
 *
 * The request is the newcomer's pull. It carries no fields (the core's
 * `crates/lakko-contacts/src/profile_request.rs` says why: the asker is
 * already MLS-authenticated through the sender leaf, and the answer is the
 * ordinary profile envelope, which needs nothing from the request). Every
 * member that reads it answers through the same throttled, jittered growth
 * path ([ProfileRepublishScheduler.onGrowth]), so N answers to one request do
 * not land in the same second.
 *
 * ## Why the decision is injected
 *
 * WHEN to ask — after a join always; on open when some member is still
 * unnamed, at most once a day per group — is the core's
 * `should_request_profiles`, reached here as `shouldRequestProfiles` over
 * UniFFI, so the three clients cannot drift on it. This file takes it as a
 * function so the plumbing around it (the count, the store, the stamp) runs in
 * a plain JVM unit test without the `.so`; the one production caller supplies
 * the real FFI.
 */

/**
 * The frozen request-envelope domain separator, `lakko/profile-request/v1`
 * followed by one NUL byte (25 bytes). Built programmatically: a raw NUL in a
 * source file makes git treat it as binary.
 */
internal val PROFILE_REQUEST_ENVELOPE_DOMAIN: ByteArray =
    "lakko/profile-request/v1".toByteArray(Charsets.US_ASCII) + byteArrayOf(0)

/**
 * Does this application plaintext open with the profile-request domain
 * separator?
 *
 * As with the profile and group-name peeks this is a complete CLASSIFICATION
 * (though not a decode): the separator ends in a NUL byte, so no chat envelope
 * (`{`) and no legacy UTF-8 message a person typed can open with it. A
 * plaintext that does is a request whether or not it decodes; one that does
 * not decode (a bad length, an unknown version) is DROPPED and never answered,
 * never handed to the chat parser whose lossy-UTF-8 fallback would render the
 * domain string as a bubble. `internal` so the foreground classifier and the
 * background push filter share this exact test. Pure — host-testable.
 */
internal fun looksLikeProfileRequestEnvelope(bytes: ByteArray): Boolean {
    if (bytes.size < PROFILE_REQUEST_ENVELOPE_DOMAIN.size) return false
    for (i in PROFILE_REQUEST_ENVELOPE_DOMAIN.indices) {
        if (bytes[i] != PROFILE_REQUEST_ENVELOPE_DOMAIN[i]) return false
    }
    return true
}

/**
 * How many members of a group this device holds no name for, the viewer
 * excluded — the `unnamed_members` argument of the core's rule.
 *
 * "No name" is the ONE resolver's verdict ([ResolvedName.isPlaceholder]):
 * no local rename, no profile name, no verified username — the row would read
 * `Member`. A verified `@username` counts as named, because it is shown.
 *
 * @param memberKeys one entry per PERSON (the account key), the roster as the
 *   repository collapses it.
 * @param ownKey this account's own key, or `null` when it could not be read;
 *   the viewer never asks after its own name, so a `null` here merely counts
 *   one row too many, which errs toward asking.
 * @param isPlaceholder the resolver, threaded in so this stays pure.
 */
public fun countUnnamedMembers(
    memberKeys: List<ByteArray>,
    ownKey: ByteArray?,
    isPlaceholder: (ByteArray) -> Boolean,
): Int = memberKeys.count { key ->
    (ownKey == null || !key.contentEquals(ownKey)) && isPlaceholder(key)
}

/** The per-`(account, group)` record of when this device last asked. */
public interface ProfileRequestStore {
    /** When this device last asked [groupIdHex], or `null` if never. */
    public fun lastRequestedAtMs(accountIdB64: String, groupIdHex: String): ULong?

    /** Record that a request went out at [atMs]. */
    public fun markRequested(accountIdB64: String, groupIdHex: String, atMs: ULong)

    /** Forget every group's stamp for [accountIdB64] (forget device). */
    public fun deleteAccount(accountIdB64: String)

    /** Forget one group's stamp (ADR-0087 section 9). */
    public fun deleteGroup(accountIdB64: String, groupIdHex: String)
}

/** An in-memory [ProfileRequestStore] for tests and for a caller with no
 *  account yet. */
public class InMemoryProfileRequestStore : ProfileRequestStore {
    private val rows = HashMap<String, ULong>()

    override fun lastRequestedAtMs(accountIdB64: String, groupIdHex: String): ULong? =
        synchronized(rows) { rows[accountIdB64 + ":" + groupIdHex.lowercase()] }

    override fun markRequested(accountIdB64: String, groupIdHex: String, atMs: ULong) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            synchronized(rows) { rows[accountIdB64 + ":" + groupIdHex.lowercase()] = atMs }
        }
    }

    override fun deleteAccount(accountIdB64: String) {
        synchronized(rows) { rows.keys.removeAll { it.startsWith("$accountIdB64:") } }
    }

    override fun deleteGroup(accountIdB64: String, groupIdHex: String) {
        synchronized(rows) { rows.keys.removeAll { it.startsWith("$accountIdB64:") && it.endsWith(groupIdHex.lowercase()) } }
    }
}

/**
 * The persisted [ProfileRequestStore]: plain `SharedPreferences` (a timestamp
 * of a request this device made is not key material), keyed by account id
 * and group id, the stamp as text so a full `u64` survives.
 */
public class ProfileRequestPrefsStore(context: Context) : ProfileRequestStore {
    private val prefs: SharedPreferences =
        context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun key(accountIdB64: String, groupIdHex: String): String =
        KEY_PREFIX + accountIdB64 + ":" + groupIdHex.lowercase()

    override fun lastRequestedAtMs(accountIdB64: String, groupIdHex: String): ULong? =
        prefs.getString(key(accountIdB64, groupIdHex), null)?.toULongOrNull()

    override fun markRequested(accountIdB64: String, groupIdHex: String, atMs: ULong) {
        prefs.edit().putString(key(accountIdB64, groupIdHex), atMs.toString()).apply()
    }

    override fun deleteAccount(accountIdB64: String) {
        val prefix = KEY_PREFIX + accountIdB64 + ":"
        val e = prefs.edit()
        for (k in prefs.all.keys) if (k.startsWith(prefix)) e.remove(k)
        e.apply()
    }

    override fun deleteGroup(accountIdB64: String, groupIdHex: String) {
        prefs.edit().remove(key(accountIdB64, groupIdHex)).apply()
    }

    private companion object {
        const val PREFS = "lakko-profile-requests"
        const val KEY_PREFIX = "lakko.profile-request.v1:"
    }
}

/**
 * Should this device ask [groupIdHex] to introduce itself now, and with what
 * stamp should a request that goes out be recorded?
 *
 * Reads the store, asks the injected core rule, and returns the moment to
 * record. It does NOT stamp: the caller marks the store only after the
 * publish actually went out (as [ProfileRepublishScheduler] records a publish
 * only when it was sent), so a publish that fails on a join is asked again on
 * the next open rather than silenced for a day. The cost — a slow publish and
 * a reopen inside it could ask twice — is absorbed by every member's answer
 * throttle, which is exactly what it is for.
 */
public class ProfileRequestPolicy(
    private val store: ProfileRequestStore,
    private val nowMs: () -> ULong = { System.currentTimeMillis().toULong() },
    /** The core's `shouldRequestProfiles(joinedJustNow, unnamedMembers,
     *  lastRequestedAtMs, nowMs)`; injected so the plumbing is host-testable. */
    private val rule: (Boolean, UInt, ULong?, ULong) -> Boolean,
) {
    /** The decision for one group: `null` for "do not ask", else the stamp
     *  to record with [recordSent] once the request has gone out. */
    public fun decide(
        accountIdB64: String,
        groupIdHex: String,
        joinedJustNow: Boolean,
        unnamedMembers: Int,
    ): ULong? {
        val now = nowMs()
        val last = store.lastRequestedAtMs(accountIdB64, groupIdHex)
        val ask = rule(joinedJustNow, unnamedMembers.coerceAtLeast(0).toUInt(), last, now)
        return if (ask) now else null
    }

    /** The request went out: record [atMs] as the last time this device asked. */
    public fun recordSent(accountIdB64: String, groupIdHex: String, atMs: ULong) {
        store.markRequested(accountIdB64, groupIdHex, atMs)
    }
}
