// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.channels

import android.content.Context
import android.content.SharedPreferences
import me.lakko.app.ds.DsStatement
import me.lakko.app.ds.DsStatementSigning
import me.lakko.app.ds.JournalClaimOutcome
import me.lakko.app.ds.claimOrReset

/**
 * ADR-0057 — PublisherGroup journals this device joined as a CO-publisher.
 *
 * Only the channel OWNER claims a PublisherGroup journal: the journal root is
 * derived from the claiming account's own seed, so a co-publisher's claim
 * would name the co-publisher's root and, on a journal the owner has not
 * claimed yet, squat it against the owner. A PublisherGroup carries no role
 * table and the channel record carries no owner flag (a co-publisher holds
 * publisher keys exactly like the owner), so the device remembers that it
 * asked to co-publish: marked when the request is sent and again when the
 * Welcome is joined. Kotlin mirror of iOS `CoPublishedJournals`
 * (`apps/ios/Lakko/Channels/CoPublisher.swift`).
 *
 * Keyed per ACCOUNT: the mark says that THIS account asked to co-publish on
 * this device. Until 2026-09-30 it was keyed on the journal id alone, so a
 * second account on the same phone inherited the first account's marks; a
 * channel the second account owns, co-published by the first, then never got
 * a claim from this device. With no account a read is `false` and a mark is
 * dropped (no claim can run then either: the publisher identity is unbound).
 * A journal id is a public DS address, so plain `SharedPreferences`.
 */
public interface CoPublishedJournalMarks {
    /** Remember that this device co-publishes the journal [journalIdHex]. */
    public fun mark(journalIdHex: String)

    /** Whether this device joined [journalIdHex] as a co-publisher. */
    public fun contains(journalIdHex: String): Boolean
}

/** A process-local [CoPublishedJournalMarks], for tests. */
public class InMemoryCoPublishedJournalMarks : CoPublishedJournalMarks {
    private val marks = mutableSetOf<String>()

    override fun mark(journalIdHex: String) {
        synchronized(marks) { marks += journalIdHex.lowercase() }
    }

    override fun contains(journalIdHex: String): Boolean =
        synchronized(marks) { journalIdHex.lowercase() in marks }
}

/**
 * The on-device [CoPublishedJournalMarks] of the account [account] names at
 * the time of each call: one string set per account under
 * `co-published.v2:<accountIdB64>`. The device-wide `co-published.v1` set of
 * earlier builds is never read; [deleteAccount] removes it.
 */
public class SharedPreferencesCoPublishedJournalMarks(
    private val prefs: SharedPreferences,
    private val account: () -> String?,
) : CoPublishedJournalMarks {
    override fun mark(journalIdHex: String) {
        val acct = account() ?: return
        // The gate outside this store's own lock (its order with the wipe).
        me.lakko.app.account.AccountStoreGate.write(acct) {
            synchronized(LOCK) {
                val key = keyOf(acct)
                val current = prefs.getStringSet(key, emptySet()).orEmpty()
                val hex = journalIdHex.lowercase()
                if (hex in current) return
                // A fresh set: the one `getStringSet` returns must not be mutated.
                prefs.edit().putStringSet(key, current + hex).apply()
            }
        }
    }

    override fun contains(journalIdHex: String): Boolean {
        val acct = account() ?: return false
        return journalIdHex.lowercase() in prefs.getStringSet(keyOf(acct), emptySet()).orEmpty()
    }

    /** Forget device: drop [accountIdB64]'s marks and the retired set. */
    public fun deleteAccount(accountIdB64: String) {
        synchronized(LOCK) {
            prefs.edit().remove(keyOf(accountIdB64)).remove(RETIRED_KEY).apply()
        }
    }

    private companion object {
        const val KEY_PREFIX: String = "co-published.v2:"
        const val RETIRED_KEY: String = "co-published.v1"
        val LOCK: Any = Any()

        fun keyOf(accountIdB64: String): String =
            KEY_PREFIX + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)
    }
}

/** Where the on-device marks live. */
public object CoPublishedJournals {
    private const val PREFS: String = "me.lakko.ds.co-published"

    /** The on-device store (application-scoped preferences) of the account
     *  [account] names; by default the signed-in one. */
    public fun onDevice(
        context: Context,
        account: () -> String? = { me.lakko.app.contacts.AcceptedContactsStore.boundAccountB64 },
    ): SharedPreferencesCoPublishedJournalMarks =
        SharedPreferencesCoPublishedJournalMarks(
            context.applicationContext.getSharedPreferences(PREFS, Context.MODE_PRIVATE),
            account,
        )
}

/** What [claimPublisherJournal] did. */
public enum class PublisherJournalClaim {
    /** This device co-publishes the channel: it never claims, it waits for the owner. */
    NOT_OWNER,

    /** 204: first claim, or the identical claim re-sent. */
    CLAIMED,

    /** A re-founded group's chain was reset by this root. */
    RESET,

    /** 409 on a live chain: left alone. */
    ALREADY_CLAIMED,
}

/**
 * ADR-0057 obligations 1 and 7 for a PublisherGroup journal, with the owner
 * gate in front: a device marked as a co-publisher of [journalIdHex] returns
 * [PublisherJournalClaim.NOT_OWNER] without building a root signer or sending
 * anything, whether the journal is claimed or not. Otherwise the root claims,
 * and resets on a `409` only when [refounded]. Refusals other than `409`
 * propagate. Free of the native core so the gate runs on the host JVM.
 */
public suspend fun claimPublisherJournal(
    journalId: ByteArray,
    journalIdHex: String,
    epoch: ULong,
    epochKey: ByteArray,
    refounded: Boolean,
    coPublished: CoPublishedJournalMarks,
    root: () -> DsStatementSigning,
    put: suspend (make: () -> DsStatement) -> Unit,
): PublisherJournalClaim {
    if (coPublished.contains(journalIdHex)) return PublisherJournalClaim.NOT_OWNER
    return when (claimOrReset(journalId, epoch, epochKey, root(), refounded, put = put)) {
        JournalClaimOutcome.CLAIMED -> PublisherJournalClaim.CLAIMED
        JournalClaimOutcome.RESET -> PublisherJournalClaim.RESET
        JournalClaimOutcome.ALREADY_CLAIMED -> PublisherJournalClaim.ALREADY_CLAIMED
    }
}
