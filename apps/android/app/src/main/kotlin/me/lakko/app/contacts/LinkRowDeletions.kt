// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

package me.lakko.app.contacts

import android.content.Context
import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import me.lakko.app.auth.base64UrlDecodeNoPad
import me.lakko.app.auth.base64UrlEncodeNoPad
import me.lakko.app.discovery.RevokeInviteResult

/*
 * ADR-0079 section 4 - the ledger holds ONE row per invite link, and only the
 * device that revokes the link deletes it, once.
 *
 * The device that revokes marks the deletion owed BEFORE it revokes the link
 * in the synced register, so a crash or an unreachable ledger in between
 * leaves it owed; it is retried after every poll until the ledger answers (a
 * deleted row, or no row: 2xx or 404). Another device of the account that
 * sees the link revoked in the register sends the ledger NOTHING: it only
 * drops its own local traces of the link. The one exception is a synced
 * record written before the 2026-10-17 rework, which carries no revocation
 * seed: nobody else can delete its row, so a device holding its own record
 * of the link with a seed takes the deletion on.
 *
 * Mirror of `apps/web/src/contacts/own-links.ts` and
 * `apps/ios/Lakko/Contacts/LinkRowDeletions.swift`.
 */

/** The contact links (ledger slugs, base64url) whose ledger row THIS device
 *  revoked and has not deleted yet, per account. Slugs only: a ledger id is
 *  not a secret (the seed is). */
public interface OwedLinkRowDeletionStore {
    public fun slugs(accountIdB64: String): Set<String>

    public fun isOwed(accountIdB64: String, slug: String): Boolean = slug in slugs(accountIdB64)

    /** Record (`owed = true`) or clear an owed deletion. */
    public fun set(accountIdB64: String, slug: String, owed: Boolean)

    public fun clearForAccount(accountIdB64: String)
}

/** In-memory store (tests, and a process with no context). */
public class InMemoryOwedLinkRowDeletionStore : OwedLinkRowDeletionStore {
    private val rows = ConcurrentHashMap<String, Set<String>>()

    override fun slugs(accountIdB64: String): Set<String> = rows[accountIdB64].orEmpty()

    override fun set(accountIdB64: String, slug: String, owed: Boolean) {
        rows.compute(accountIdB64) { _, current ->
            val next = if (owed) current.orEmpty() + slug else current.orEmpty() - slug
            next.ifEmpty { null }
        }
    }

    override fun clearForAccount(accountIdB64: String) {
        rows.remove(accountIdB64)
    }
}

/** The on-device store: one plain SharedPreferences file, one string set per
 *  account. Never throws; an unreadable row reads as empty. Not written once
 *  the account's session has ended ([me.lakko.app.account.AccountStoreGate]).
 *  Cleared by forget device ([clear]). */
public class SharedPrefsOwedLinkRowDeletionStore(
    private val prefsProvider: () -> android.content.SharedPreferences,
) : OwedLinkRowDeletionStore {
    private val prefs: android.content.SharedPreferences by lazy { prefsProvider() }

    override fun slugs(accountIdB64: String): Set<String> = try {
        prefs.getStringSet(storageKey(accountIdB64), null)?.toSet().orEmpty()
    } catch (_: Throwable) {
        emptySet()
    }

    override fun set(accountIdB64: String, slug: String, owed: Boolean) {
        me.lakko.app.account.AccountStoreGate.write(accountIdB64) {
            try {
                val current = slugs(accountIdB64)
                if (owed == (slug in current)) return@write
                val next = if (owed) current + slug else current - slug
                val editor = prefs.edit()
                if (next.isEmpty()) editor.remove(storageKey(accountIdB64)) else editor.putStringSet(storageKey(accountIdB64), next)
                // At once: the mark must be on disk before the register changes.
                editor.commit()
            } catch (_: Throwable) {
                // Best effort.
            }
        }
    }

    override fun clearForAccount(accountIdB64: String) {
        try {
            prefs.edit().remove(storageKey(accountIdB64)).commit()
        } catch (_: Throwable) {
            // best effort
        }
    }

    public companion object {
        public const val PREFS_NAME: String = "lakko-link-row-deletions-owed"

        private const val KEY_PREFIX: String = "owed-v1:"

        internal fun storageKey(accountIdB64: String): String = KEY_PREFIX + accountIdB64

        /** The store over the app's own file. */
        public fun of(context: Context): SharedPrefsOwedLinkRowDeletionStore {
            val app = context.applicationContext
            return SharedPrefsOwedLinkRowDeletionStore { app.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE) }
        }

        /** Forget device: drop the account's owed list. */
        public fun clear(context: Context, accountIdB64: String) {
            of(context).clearForAccount(accountIdB64)
        }
    }
}

/**
 * Deletes the ledger row of a contact link this device revoked, exactly once.
 *
 * A deletion runs only while it is owed ([owed]); a call while the same
 * deletion is running joins it instead of sending a second `DELETE`, and a
 * call once it is no longer owed sends nothing. The owed mark is cleared once
 * the ledger answered (row deleted, or 404: gone, expired, revoked earlier)
 * and kept when the ledger could not be reached. Any other refusal is
 * rethrown and the deletion stays owed.
 */
public class LinkRowDeleter(public val owed: OwedLinkRowDeletionStore) {

    /** How one deletion ended. */
    public sealed interface Outcome {
        /** This call (or the call it joined) got the ledger's answer. */
        public data class Deleted(val result: RevokeInviteResult) : Outcome

        /** Not owed any more: another call on this device already got the
         *  ledger's answer. Nothing was sent. */
        public data object AlreadyDone : Outcome

        /** The ledger could not be reached; the deletion stays owed. */
        public data object Unreachable : Outcome

        /** No revocation seed is known for the row; nobody on this device can
         *  delete it (the owed mark is dropped). */
        public data object NoKey : Outcome
    }

    /** The ledger call: `DiscoveryClient.revokeInvite(blobId, seed)` in
     *  production. */
    public fun interface Delete {
        public suspend fun delete(blobId: ByteArray, seed: ByteArray): RevokeInviteResult
    }

    private val lock = Mutex()
    private val inFlight = HashMap<String, CompletableDeferred<Result<Outcome>>>()

    /** Serialises sweeps with one another ([LinkRowSweep]). */
    internal val sweepLock: Mutex = Mutex()

    /** Mark the deletion of [slug] owed (the revoking device, before the
     *  register changes). */
    public suspend fun markOwed(accountIdB64: String, slug: String) {
        lock.withLock { owed.set(accountIdB64, slug, true) }
    }

    /** Undo [markOwed] when the register revoke failed; a deletion already
     *  running keeps its mark. */
    public suspend fun clearOwedIfIdle(accountIdB64: String, slug: String) {
        lock.withLock { if (key(accountIdB64, slug) !in inFlight) owed.set(accountIdB64, slug, false) }
    }

    /** Take a deletion on (the seedless exception): mark it owed when it is
     *  neither owed nor running and [held] still holds. Decided under the same
     *  lock that ends a deletion, so a deletion that just finished (and
     *  dropped the local record [held] reads) is never taken on again. */
    public suspend fun claimIfHeld(accountIdB64: String, slug: String, held: () -> Boolean): Boolean =
        lock.withLock {
            if (key(accountIdB64, slug) in inFlight || owed.isOwed(accountIdB64, slug) || !held()) {
                false
            } else {
                owed.set(accountIdB64, slug, true)
                true
            }
        }

    /** Whether the deletion of [slug] is owed or running now. */
    public suspend fun isOwedOrRunning(accountIdB64: String, slug: String): Boolean =
        lock.withLock { key(accountIdB64, slug) in inFlight || owed.isOwed(accountIdB64, slug) }

    /**
     * Run the owed deletion of the row at [blobId], once (see the type).
     * [onAnswered] runs once the ledger answered (or no seed is known), BEFORE
     * the owed mark is cleared: the caller drops its local traces there, so a
     * sweep that looks after the mark is gone finds no record to act on.
     */
    public suspend fun deleteOwed(
        accountIdB64: String,
        blobId: ByteArray,
        seed: ByteArray?,
        delete: Delete,
        onAnswered: suspend () -> Unit = {},
    ): Outcome {
        val slug = base64UrlEncodeNoPad(blobId)
        val key = key(accountIdB64, slug)
        val (deferred, owner) = lock.withLock {
            val running = inFlight[key]
            if (running != null) {
                running to false
            } else {
                if (!owed.isOwed(accountIdB64, slug)) return Outcome.AlreadyDone
                val fresh = CompletableDeferred<Result<Outcome>>()
                inFlight[key] = fresh
                fresh to true
            }
        }
        if (!owner) return deferred.await().getOrThrow()

        // Unreachable unless the ledger answered: a cancelled call leaves the
        // deletion owed for the next sweep.
        var outcome: Result<Outcome> = Result.success(Outcome.Unreachable)
        var answered = false
        try {
            outcome = if (seed == null || seed.size != SEED_LEN) {
                answered = true
                Result.success(Outcome.NoKey)
            } else {
                try {
                    val result = delete.delete(blobId, seed)
                    answered = true
                    Result.success(Outcome.Deleted(result))
                } catch (e: kotlinx.coroutines.CancellationException) {
                    throw e
                } catch (e: Throwable) {
                    if (isUnreachable(e)) Result.success(Outcome.Unreachable) else Result.failure(e)
                }
            }
        } finally {
            withContext(NonCancellable) {
                if (answered) {
                    try {
                        onAnswered()
                    } catch (_: Throwable) {
                        // The local traces go at the next sweep.
                    }
                }
                lock.withLock {
                    if (answered) owed.set(accountIdB64, slug, false)
                    inFlight.remove(key)
                }
                deferred.complete(outcome)
            }
        }
        return outcome.getOrThrow()
    }

    private fun key(accountIdB64: String, slug: String): String = "$accountIdB64|$slug"

    public companion object {
        private const val SEED_LEN: Int = 32

        /** A transport failure: no answer from the ledger. Looks through the
         *  repository wrapper. */
        public fun isUnreachable(error: Throwable): Boolean {
            var e: Throwable? = error
            var depth = 0
            while (e != null && depth < 8) {
                if (e is me.lakko.app.discovery.DiscoveryNetworkError || e is java.io.IOException) return true
                e = e.cause
                depth += 1
            }
            return false
        }
    }
}

/** Revoking one contact link on this device (the device that revokes deletes
 *  the row). */
public object LinkRowRevoke {
    /**
     * Mark the deletion owed, revoke the link in the register
     * ([revokeRegister], `null` when the register is not open or holds no
     * record of the link), then delete the row once. A seed is taken from the
     * register's record ([recordSeed]) or this device's own record
     * ([ownSeed]); with neither, the register is still revoked and nothing is
     * owed ([LinkRowDeleter.Outcome.NoKey]). A failed register revoke clears
     * the mark and is rethrown. [forget] drops the local traces once the
     * ledger answered.
     */
    public suspend fun run(
        accountIdB64: String,
        blobId: ByteArray,
        recordSeed: ByteArray?,
        ownSeed: ByteArray?,
        revokeRegister: (suspend () -> Unit)?,
        deleter: LinkRowDeleter,
        delete: LinkRowDeleter.Delete,
        forget: suspend (ByteArray) -> Unit,
    ): LinkRowDeleter.Outcome {
        val seed = recordSeed?.takeIf { it.size == 32 } ?: ownSeed?.takeIf { it.size == 32 }
        if (seed == null) {
            revokeRegister?.invoke()
            return LinkRowDeleter.Outcome.NoKey
        }
        val slug = base64UrlEncodeNoPad(blobId)
        deleter.markOwed(accountIdB64, slug)
        if (revokeRegister != null) {
            try {
                revokeRegister()
            } catch (e: Throwable) {
                deleter.clearOwedIfIdle(accountIdB64, slug)
                throw e
            }
        }
        return deleter.deleteOwed(accountIdB64, blobId, seed, delete) { forget(blobId) }
    }
}

/** The post-poll sweep of revoked contact links. */
public object LinkRowSweep {
    /** A contact link the synced register holds as revoked. */
    public class RevokedLink(
        public val blobId: ByteArray,
        /** The seed the register carries; empty for a record written before
         *  the 2026-10-17 rework. */
        public val recordSeed: ByteArray,
    ) {
        public val slug: String get() = base64UrlEncodeNoPad(blobId)
    }

    /** What the sweep did, for tests and logs. */
    public data class Report(
        var retried: Int = 0,
        var unreachable: Int = 0,
        var forgotten: Int = 0,
    )

    /**
     * 1. Retry every deletion this device owes (it revoked the link).
     * 2. A link revoked by another device of the account: the revoking device
     *    deletes the row itself, so this device sends the ledger NOTHING and
     *    only drops its local traces ([forget]). The one exception is a record
     *    with no seed, whose row no other device can delete: when this device
     *    holds its own record of the link with a seed ([ownSeed]), it takes
     *    the deletion on (marks it owed and runs it once).
     *
     * [forget] must be idempotent and cheap when there is nothing to drop.
     * Sweeps run one at a time. Never throws but for cancellation.
     */
    public suspend fun run(
        accountIdB64: String,
        revoked: List<RevokedLink>,
        ownSeed: (slug: String) -> ByteArray?,
        deleter: LinkRowDeleter,
        delete: LinkRowDeleter.Delete,
        forget: suspend (ByteArray) -> Unit,
    ): Report = deleter.sweepLock.withLock {
        val report = Report()
        val bySlug = revoked.associateBy { it.slug }

        suspend fun runOwed(slug: String, blobId: ByteArray) {
            val recordSeed = bySlug[slug]?.recordSeed
            val seed = recordSeed?.takeIf { it.size == 32 } ?: ownSeed(slug)
            report.retried += 1
            try {
                val outcome = deleter.deleteOwed(accountIdB64, blobId, seed, delete) { forget(blobId) }
                if (outcome == LinkRowDeleter.Outcome.Unreachable) report.unreachable += 1
            } catch (e: kotlinx.coroutines.CancellationException) {
                throw e
            } catch (_: Throwable) {
                // Refused: kept owed, tried again after the next poll.
            } finally {
                if (seed != null && seed !== recordSeed) seed.fill(0)
            }
        }

        // 1. Owed deletions, sorted for a stable order.
        for (slug in deleter.owed.slugs(accountIdB64).sorted()) {
            val blobId = runCatching { base64UrlDecodeNoPad(slug) }.getOrNull()
            if (blobId == null || blobId.isEmpty()) {
                deleter.owed.set(accountIdB64, slug, false)
                continue
            }
            runOwed(slug, blobId)
        }

        // 2. Revoked elsewhere.
        for (link in revoked) {
            val slug = link.slug
            if (deleter.isOwedOrRunning(accountIdB64, slug)) continue
            if (link.recordSeed.size != 32 &&
                deleter.claimIfHeld(accountIdB64, slug) { ownSeed(slug)?.also { it.fill(0) }?.size == 32 }
            ) {
                runOwed(slug, link.blobId)
                continue
            }
            forget(link.blobId)
            report.forgotten += 1
        }
        report
    }
}
