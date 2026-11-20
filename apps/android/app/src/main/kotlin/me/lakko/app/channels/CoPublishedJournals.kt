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

    /**
     * ADR-0078 section 6 — [mark] that also remembers which channel the
     * journal is. A PublisherGroup id is the OWNER's derivation over its
     * account seed and the channel id, which a co-publisher cannot compute,
     * so this is how it opens the group it joined ([journalIdOf]).
     */
    public fun markChannel(journalIdHex: String, channelIdHex: String)

    /** The PublisherGroup id hex of [channelIdHex] when this account
     *  co-publishes it, else `null` (it owns the channel, or does not
     *  publish). */
    public fun journalIdOf(channelIdHex: String): String?

    /** ADR-0090 section 2.3 — record the channel owner's account key this
     *  account verified as the `/g/` capability's issuer (or read from the
     *  account's register): the only key a co-publisher device takes a
     *  PublisherGroup Welcome from. */
    public fun markOwner(channelIdHex: String, ownerAccountKey: ByteArray) {}

    /** The recorded owner account key of a co-published [channelIdHex]. */
    public fun ownerKeyOf(channelIdHex: String): ByteArray? = null
}

/** A process-local [CoPublishedJournalMarks], for tests. */
public class InMemoryCoPublishedJournalMarks : CoPublishedJournalMarks {
    private val marks = mutableSetOf<String>()
    private val channels = mutableMapOf<String, String>()

    override fun mark(journalIdHex: String) {
        synchronized(marks) { marks += journalIdHex.lowercase() }
    }

    override fun contains(journalIdHex: String): Boolean =
        synchronized(marks) { journalIdHex.lowercase() in marks }

    override fun markChannel(journalIdHex: String, channelIdHex: String) {
        synchronized(marks) {
            marks += journalIdHex.lowercase()
            channels[channelIdHex.lowercase()] = journalIdHex.lowercase()
        }
    }

    override fun journalIdOf(channelIdHex: String): String? =
        synchronized(marks) { channels[channelIdHex.lowercase()] }

    private val owners = mutableMapOf<String, ByteArray>()

    override fun markOwner(channelIdHex: String, ownerAccountKey: ByteArray) {
        if (ownerAccountKey.size != 32) return
        synchronized(marks) { owners[channelIdHex.lowercase()] = ownerAccountKey.copyOf() }
    }

    override fun ownerKeyOf(channelIdHex: String): ByteArray? =
        synchronized(marks) { owners[channelIdHex.lowercase()]?.copyOf() }
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

    override fun markChannel(journalIdHex: String, channelIdHex: String) {
        mark(journalIdHex)
        val acct = account() ?: return
        me.lakko.app.account.AccountStoreGate.write(acct) {
            synchronized(LOCK) {
                val key = channelsKeyOf(acct)
                val channel = channelIdHex.lowercase()
                val current = prefs.getStringSet(key, emptySet()).orEmpty()
                val next = current.filterNot { it.substringBefore(':') == channel }.toSet() +
                    "$channel:${journalIdHex.lowercase()}"
                prefs.edit().putStringSet(key, next).apply()
            }
        }
    }

    override fun journalIdOf(channelIdHex: String): String? {
        val acct = account() ?: return null
        val channel = channelIdHex.lowercase()
        return prefs.getStringSet(channelsKeyOf(acct), emptySet()).orEmpty()
            .firstOrNull { it.substringBefore(':') == channel }
            ?.substringAfter(':')
    }

    override fun markOwner(channelIdHex: String, ownerAccountKey: ByteArray) {
        if (ownerAccountKey.size != 32) return
        val acct = account() ?: return
        me.lakko.app.account.AccountStoreGate.write(acct) {
            synchronized(LOCK) {
                val key = ownersKeyOf(acct)
                val channel = channelIdHex.lowercase()
                val hex = ownerAccountKey.joinToString("") { "%02x".format(it) }
                val current = prefs.getStringSet(key, emptySet()).orEmpty()
                val next = current.filterNot { it.substringBefore(':') == channel }.toSet() + "$channel:$hex"
                prefs.edit().putStringSet(key, next).apply()
            }
        }
    }

    override fun ownerKeyOf(channelIdHex: String): ByteArray? {
        val acct = account() ?: return null
        val channel = channelIdHex.lowercase()
        val hex = prefs.getStringSet(ownersKeyOf(acct), emptySet()).orEmpty()
            .firstOrNull { it.substringBefore(':') == channel }
            ?.substringAfter(':')
            ?.takeIf { it.length == 64 } ?: return null
        return ByteArray(32) { i -> hex.substring(i * 2, i * 2 + 2).toInt(16).toByte() }
    }

    /** Forget device: drop [accountIdB64]'s marks and the retired set. */
    public fun deleteAccount(accountIdB64: String) {
        synchronized(LOCK) {
            prefs.edit()
                .remove(keyOf(accountIdB64))
                .remove(channelsKeyOf(accountIdB64))
                .remove(ownersKeyOf(accountIdB64))
                .remove(RETIRED_KEY)
                .apply()
        }
    }

    private companion object {
        const val KEY_PREFIX: String = "co-published.v2:"
        const val CHANNELS_KEY_PREFIX: String = "co-published-channels.v1:"
        const val RETIRED_KEY: String = "co-published.v1"
        val LOCK: Any = Any()

        fun keyOf(accountIdB64: String): String =
            KEY_PREFIX + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)

        fun channelsKeyOf(accountIdB64: String): String =
            CHANNELS_KEY_PREFIX + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)

        /** ADR-0090 — `channel:ownerKeyHex` entries per account. */
        fun ownersKeyOf(accountIdB64: String): String =
            "co-published-owners.v1:" + me.lakko.app.contacts.AcceptedContactsStore.canonicalAccountB64(accountIdB64)
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
 * anything, whether the journal is claimed or not. Otherwise the root claims;
 * a `409` is left alone ([PublisherJournalClaim.ALREADY_CLAIMED]) and the
 * journal is NEVER reset here: only New channel founds a group, and it
 * resets in its own claim (docs/ux/second-device.md section 4a, the trap
 * rule). Refusals other than `409` propagate. Free of the native core so the
 * gate runs on the host JVM.
 */
public suspend fun claimPublisherJournal(
    journalId: ByteArray,
    journalIdHex: String,
    epoch: ULong,
    epochKey: ByteArray,
    coPublished: CoPublishedJournalMarks,
    root: () -> DsStatementSigning,
    put: suspend (make: () -> DsStatement) -> Unit,
): PublisherJournalClaim {
    if (coPublished.contains(journalIdHex)) return PublisherJournalClaim.NOT_OWNER
    return when (claimOrReset(journalId, epoch, epochKey, root(), refounded = false, put = put)) {
        JournalClaimOutcome.CLAIMED -> PublisherJournalClaim.CLAIMED
        JournalClaimOutcome.RESET -> PublisherJournalClaim.RESET
        JournalClaimOutcome.ALREADY_CLAIMED -> PublisherJournalClaim.ALREADY_CLAIMED
    }
}
